// SPDX-License-Identifier: Apache-2.0
//! The word a command runs, judged the way the shell reads it.
//!
//! - A command word the shell only knows when it runs (an expansion, a
//!   substitution, a glob or a brace expansion: `$V`, `${V:-x}`, `x?y`) names
//!   no program the gate can judge, so it is refused.
//! - A function that reads the arguments of its call (`$1`, `"$@"`, `$*`)
//!   runs like an alias body: whatever a later call passes reaches the
//!   commands inside it. Such a function is accepted only when each of its
//!   commands starts with a program the gate does not judge by its arguments
//!   (the rule an alias body meets, [`super::enforcement`]), and a command
//!   whose arguments are positional parameters meets the same rule.

use lean_ctx::core::error::ShellError;

use super::compound::{balanced_brace_inner, balanced_paren_at};
use super::ps_statements::{Mode, host_mode};
use super::tokenizer::{detect_function_def, exec_name, function_body};
use super::{expand_to_leaf_segments, extract_all_commands, shell_tokenize, skip_env_assignments};

/// Words that run no other command and read their arguments as plain data:
/// they may take the arguments of a call.
const DATA_ONLY_WORDS: &[&str] = &[
    "echo", "printf", "true", "false", ":", "test", "[", "[[", "return", "exit", "shift", "local",
    "getopts", "cd", "pwd",
];

/// Reserved words that open, join or close a compound command and run
/// nothing themselves (a `for` header's words are data); the words that run
/// the next one are skipped first ([`INTRO_WORDS`]).
const CONSTRUCT_WORDS: &[&str] = &[
    "fi", "for", "select", "in", "done", "case", "esac", "function", "}", "]]",
];

/// Whether `text` reads a positional parameter (`$1`, `$@`, `$*`, `${2}`,
/// `${@:1}`, any indirect `${!…}`, zsh's `$argv`, `$OPTARG`) outside single quotes, or
/// walks them without naming them ([`has_implicit_argument_loop`]).
pub(super) fn reads_positional_parameters(text: &str) -> bool {
    reads_positional_references(text) || has_implicit_argument_loop(text)
}

/// Whether `text` reads the arguments of a call without naming them: a
/// `for`/`select` loop with no `in` list walks `"$@"`, and `getopts` with
/// no words of its own parses `"$@"` into `$OPTARG`.
pub(super) fn has_implicit_argument_loop(text: &str) -> bool {
    let words = shell_tokenize(text);
    words.iter().enumerate().any(|(i, w)| match w.as_str() {
        "for" | "select" => words.get(i + 1).is_some_and(|name| {
            let bare = name.strip_suffix(';').unwrap_or(name);
            is_identifier(bare)
                && (bare.len() < name.len()
                    || words.get(i + 2).is_none_or(|next| next.as_str() == "do"))
        }),
        "getopts" => true,
        _ => false,
    })
}

/// A shell variable name.
fn is_identifier(word: &str) -> bool {
    word.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// [`reads_positional_parameters`] for the `$…` references.
fn reads_positional_references(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut single = false;
    let mut double = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if single {
            single = c != '\'';
            i += 1;
            continue;
        }
        match c {
            '\\' => i += 1,
            '\'' if !double => single = true,
            '"' => double = !double,
            '$' if names_positional(&chars[i + 1..]) => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Whether the text after a `$` names a positional parameter.
fn names_positional(rest: &[char]) -> bool {
    let mut rest = rest;
    if rest.first() == Some(&'{') {
        rest = &rest[1..];
        // An indirect expansion (`${!#}`, `${!n}`) may name any parameter,
        // a positional one too.
        if rest.first() == Some(&'!') {
            return true;
        }
        if rest.first() == Some(&'#') {
            rest = &rest[1..];
        }
    }
    match rest.first() {
        Some('@' | '*') => true,
        Some(c) if c.is_ascii_digit() => *c != '0',
        _ => {
            let name: String = rest
                .iter()
                .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
                .collect();
            matches!(name.as_str(), "argv" | "BASH_ARGV" | "OPTARG")
        }
    }
}

/// Whether the command word `word` (quotes removed) is only known when the
/// shell runs: it holds an expansion or substitution (`$`, a backtick), a
/// glob (`*`, `?`, `[…]`, an extended glob) or a brace expansion
/// (`{a,b}`, `{1..3}`). `[`, `[[`, `{`, an arithmetic command `((…))` and an
/// array element assignment (`a[0]=x`) are none.
pub(super) fn is_dynamic_command_word(word: &str) -> bool {
    if matches!(word, "[" | "[[" | "{" | "{}") || word.starts_with("((") {
        return false;
    }
    if is_array_element_assignment(word) {
        return false;
    }
    word.contains(['$', '`', '*', '?', '['])
        || ["@(", "+(", "!("].iter().any(|g| word.contains(g))
        || (word.contains('{') && (word.contains(',') || word.contains("..")))
}

/// `name[subscript]=value` / `name[subscript]+=value`.
fn is_array_element_assignment(word: &str) -> bool {
    let Some(open) = word.find('[') else {
        return false;
    };
    let name = &word[..open];
    let starts_well = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_well
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (word[open..].contains("]=") || word[open..].contains("]+="))
}

/// A case pattern that opens a segment (`*)`, `(a|b)`, `-h)`): the command
/// follows it.
fn is_case_pattern(token: &str) -> bool {
    token.ends_with(')')
        && !token.starts_with("((")
        && !token.starts_with("$(")
        && !token.contains('`')
}

/// Reserved words and modifiers the shell runs the next word after.
const INTRO_WORDS: &[&str] = &[
    "{", "if", "then", "else", "elif", "do", "while", "until", "!", "time", "coproc",
];

/// The command word of `segment` as written (path kept, quotes removed),
/// behind assignments, a brace-group `{`, reserved words that run the next
/// word ([`INTRO_WORDS`], also `time -p --` and the assignments after
/// them) and case patterns.
fn command_word(segment: &str) -> Option<String> {
    // `time`'s own options stay behind when a caller peeled `time` off.
    shell_tokenize(skip_env_assignments(segment.trim()))
        .into_iter()
        .find(|t| {
            let word = t.as_str();
            !INTRO_WORDS.contains(&word)
                && !matches!(word, "-p" | "--")
                && !is_assignment_word(word)
                && !is_case_pattern(t)
        })
}

/// `NAME=value`, `NAME+=value` or an array element assignment.
fn is_assignment_word(word: &str) -> bool {
    word.split_once('=')
        .is_some_and(|(name, _)| is_identifier(name.strip_suffix('+').unwrap_or(name)))
        || is_array_element_assignment(word)
}

/// The body of a function definition segment, also when a redirection
/// follows its closing `}` or `)` (`f(){ …; } >/dev/null`): the redirection
/// applies to every call, and the body is what runs.
pub(super) fn definition_body(segment: &str) -> Option<&str> {
    if let Some(body) = function_body(segment) {
        return Some(body);
    }
    // A definition with a suffix is no definition to `function_body`: the
    // text up to the last closer is tried instead.
    let text = segment.trim_end();
    ['}', ')']
        .iter()
        .filter_map(|closer| text.rfind(*closer))
        .find_map(|end| function_body(&text[..=end]))
}

/// The commands of a `( … )` subshell that makes up `segment`, also behind
/// a redirection; an arithmetic command `((…))` is none.
fn subshell_inner(segment: &str) -> Option<&str> {
    let text = segment.trim();
    if !text.starts_with('(') || (text.starts_with("((") && text.ends_with("))")) {
        return None;
    }
    balanced_paren_at(text, 0).map(|(inner, _)| inner.trim())
}

/// Without an allowlist (blocklist-only mode): every command of `command`
/// gets [`check_command_word`] and [`check_positional_forwarding`]. The
/// commands are the leaves of the line's compound commands; a line the leaf
/// walk cannot read is split at its operators and brace groups instead.
pub(super) fn check_line(command: &str) -> Result<(), ShellError> {
    if host_mode() == Mode::PowerShell {
        return Ok(());
    }
    check_implicit_arguments(command)?;
    check_words(command, 0)
}

/// A line that walks the arguments without naming them (`set -- …; for a;
/// do …; done`): every command in it is judged like a function body that
/// takes the arguments of its call ([`check_function_forwarding`]).
pub(super) fn check_implicit_arguments(command: &str) -> Result<(), ShellError> {
    if host_mode() == Mode::PowerShell || !has_implicit_argument_loop(command) {
        return Ok(());
    }
    check_function_forwarding(command, 0)
}

/// [`check_line`] at a function-body depth: the command words in a body
/// are judged like those of the line, whether or not it reads arguments.
fn check_words(command: &str, depth: usize) -> Result<(), ShellError> {
    if depth > MAX_FUNCTION_DEPTH {
        return Err(too_deep_functions());
    }
    let leaves = expand_to_leaf_segments(command).unwrap_or_else(|_| {
        let mut segments = Vec::new();
        collect_segments(command, 0, &mut segments);
        segments
    });
    for leaf in &leaves {
        check_command_word(leaf)?;
        check_positional_forwarding(leaf)?;
        // A command in a function body or subshell runs like one of the line:
        // the interpreter checks judge it too (`f(){ cmd /c …; }`,
        // `f(){ ./x.bat; }`); the line's own commands had them already.
        if depth > 0 {
            super::enforcement::check_interpreter_eval_only(leaf)?;
        }
        if let Some(body) = definition_body(leaf) {
            check_words(body, depth + 1)?;
        } else if let Some(inner) = subshell_inner(leaf) {
            check_words(inner, depth + 1)?;
        }
    }
    Ok(())
}

fn too_deep_functions() -> ShellError {
    "[BLOCKED — DO NOT RETRY] Function definitions or subshells are nested too deeply to \
     check. Define functions one after another and run subshells directly."
        .into()
}

/// The operator-separated segments of `command`, also those inside a brace
/// group that makes up a whole segment.
fn collect_segments(command: &str, depth: usize, out: &mut Vec<String>) {
    for segment in extract_all_commands(command) {
        if depth < MAX_FUNCTION_DEPTH
            && let Some(inner) = balanced_brace_inner(&segment)
        {
            collect_segments(inner, depth + 1, out);
        }
        out.push(segment);
    }
}

/// Refuses a segment whose command word is only known when the shell runs
/// ([`is_dynamic_command_word`]). A PowerShell host reads `$name` as a
/// variable, not as a command word; its own checks judge it.
pub(super) fn check_command_word(segment: &str) -> Result<(), ShellError> {
    if host_mode() == Mode::PowerShell {
        return Ok(());
    }
    match command_word(segment) {
        Some(word) if is_dynamic_command_word(&word) => Err(dynamic_word_refusal(&word)),
        _ => Ok(()),
    }
}

/// The refusal for a command word the shell builds when it runs.
pub(super) fn dynamic_word_refusal(word: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] The command word '{word}' is an expansion, substitution or \
         pattern: the shell decides only when it runs which program it names, so it cannot \
         be gated. Write the program name directly."
    )
    .into()
}

/// Whether a command that starts with `word` may take the arguments of a
/// call: `word` is a plain program name, no path, and no builtin, reserved
/// word or program the gate judges by its arguments — or a word that reads
/// its arguments as data ([`DATA_ONLY_WORDS`]).
fn forwarding_word_is_refused(word: &str) -> bool {
    if CONSTRUCT_WORDS.contains(&word) {
        return false;
    }
    if is_dynamic_command_word(word) || word.contains(['/', '\\']) {
        return true;
    }
    let name = exec_name(word).to_ascii_lowercase();
    !DATA_ONLY_WORDS.contains(&name.as_str()) && super::enforcement::alias_word_is_judged(&name)
}

fn forwarding_refusal(word: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] '{word}' takes the arguments of a call (`$1`, `\"$@\"`, \
         `$*`): a path, a builtin, an interpreter, a wrapper or a program the gate judges by \
         its arguments may not take them, because the gate cannot see what a later call \
         passes. Run the command directly with its arguments."
    )
    .into()
}

/// A segment that is no function definition and whose arguments read the
/// positional parameters: its command word must meet
/// [`forwarding_word_is_refused`]. A function definition gets
/// [`check_function_forwarding`].
pub(super) fn check_positional_forwarding(segment: &str) -> Result<(), ShellError> {
    if host_mode() == Mode::PowerShell {
        return Ok(());
    }
    if let Some(body) = definition_body(segment) {
        return check_function_forwarding(body, 0);
    }
    if detect_function_def(segment).is_some() || !reads_positional_parameters(segment) {
        return Ok(());
    }
    match command_word(segment) {
        Some(word) if forwarding_word_is_refused(&word) => Err(forwarding_refusal(&word)),
        _ => Ok(()),
    }
}

/// Nested function definitions and subshells followed inside a body.
const MAX_FUNCTION_DEPTH: usize = 4;

/// A function body that reads the positional parameters: every command in
/// it (a value copied from `$1` reaches the next one too) must start with a
/// word [`forwarding_word_is_refused`] accepts. A body that does not read
/// them takes nothing from a call. A body whose commands cannot be read is
/// refused.
pub(super) fn check_function_forwarding(body: &str, depth: usize) -> Result<(), ShellError> {
    if depth > MAX_FUNCTION_DEPTH {
        return Err(too_deep_functions());
    }
    if !reads_positional_parameters(body) {
        return Ok(());
    }
    let leaves = expand_to_leaf_segments(body).map_err(|_| {
        ShellError::from(
            "[BLOCKED — DO NOT RETRY] A function that takes the arguments of its call has a \
             body whose commands cannot be read, so the gate cannot tell what a later call \
             runs. Run the commands directly."
                .to_string(),
        )
    })?;
    // `for a do cmd …` keeps `cmd` behind `do` in a leaf the loop header
    // starts: the word after each `do` is judged too.
    let words = shell_tokenize(body);
    for pair in words.windows(2) {
        if pair[0] == "do" && forwarding_word_is_refused(&pair[1]) {
            return Err(forwarding_refusal(&pair[1]));
        }
    }
    for leaf in &leaves {
        if let Some(inner) = definition_body(leaf) {
            check_function_forwarding(inner, depth + 1)?;
            continue;
        }
        if let Some(word) = command_word(leaf)
            && forwarding_word_is_refused(&word)
        {
            return Err(forwarding_refusal(&word));
        }
    }
    Ok(())
}
