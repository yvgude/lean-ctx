// SPDX-License-Identifier: Apache-2.0
//! Dataflow across the statements of one command line (#35).
//!
//! Every other check reads one segment at a time. Bash, though, carries
//! values from one statement to the next, and two of its sinks run code
//! that a value spells as text:
//! - arithmetic evaluates a variable's value again, so `X='a[$(cmd)]';
//!   : $((X))` runs `cmd` (also `((X))`, `let X`, `[[ X -eq 0 ]]`,
//!   `${a[X]}`, `${s:X}`, `a[X]=`, and zsh's `exit`/`return`/`shift X`);
//! - `declare`/`export`/`local`/`readonly NAME=$V` re-parses `$V` as a
//!   compound assignment when `NAME` is an array (`PIPESTATUS`, or one the
//!   line made), so `P='($(cmd))'; declare PIPESTATUS=$P` runs `cmd`.
//!
//! `wait` reads its options after expansion: `o='-pa[$(cmd)]'; wait -n "$o"`
//! assigns through an evaluated subscript, `o=-pBASH_ENV` sets BASH_ENV. A
//! word built at run time where `wait` reads an option must be a number
//! (`$!`, `$((…))`, a variable the line assigns only numbers, `pid=$!`).
//!
//! The check is flow-insensitive over the whole line: a variable is tainted
//! when any statement gives it a value that arithmetic could expand (a
//! subscript, a `$`, a backtick, a glob, input from `read`/`mapfile`/
//! `printf -v`/`getopts`/`select`, a command's output other than a known
//! number), or a value naming a tainted variable. A sink that names a
//! tainted variable is refused.
//!
//! Not covered: the environment a command starts with (the ctx_shell `env`
//! parameter) is trusted, as for every other check.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use super::ShellError;
use super::compound::expand_to_leaf_segments;
use super::heredoc::strip_for_analysis;
use super::lexer::lex;
use super::operand_eval::{
    Piece, PrintfTarget, backtick_end, builds_text, expansion_end, is_arithmetic, matching_close,
    may_become_option, printf_target, raw_words, word_pieces,
};
use super::tokenizer::{
    Separator, detect_function_def, extract_command_tokens_from_segment, function_body,
    segments_with_separators,
};

/// The name standing for the positional parameters (`$1`, `$@`, `$*`).
const POSITIONAL: &str = "@";

/// Variables bash keeps as arrays: assigning one through `declare`-family
/// builtins re-parses a value that expands to `(…)` as a compound assignment.
const KNOWN_ARRAYS: &[&str] = &[
    "PIPESTATUS",
    "BASH_ALIASES",
    "BASH_CMDS",
    "DIRSTACK",
    "GROUPS",
    "FUNCNAME",
    "BASH_ARGV",
    "BASH_ARGC",
    "BASH_LINENO",
    "BASH_SOURCE",
    "BASH_REMATCH",
    "BASH_VERSINFO",
    "COMP_WORDS",
    "COPROC",
    "MAPFILE",
];

/// Variables whose value comes from data the line does not show: `$_` (the
/// last argument of the previous command), the groups of the last `=~`, and
/// the text of the command running (which holds any quoted `a[$(cmd)]`).
const ALWAYS_TAINTED: &[&str] = &["_", "BASH_REMATCH", "BASH_COMMAND", "BASH_EXECUTION_STRING"];

/// Variables a directory change sets from its operand: `cd 'a[$(cmd)]'`
/// puts that text into them.
const DIRECTORY_VARIABLES: &[&str] = &["PWD", "OLDPWD", "DIRSTACK"];

const DECLARATION_BUILTINS: &[&str] = &["export", "declare", "typeset", "local", "readonly"];

/// Deepest substitution nesting followed; the lexer refuses far less.
const MAX_DEPTH: usize = 16;

#[derive(Clone)]
enum Value {
    /// Arithmetic could expand something in it.
    Unsafe,
    /// Only numbers, operators and these names.
    Refs(Vec<String>),
}

#[derive(Default)]
struct Flow {
    values: HashMap<String, Vec<Value>>,
    opaque: HashSet<String>,
    arrays: HashSet<String>,
    /// Arithmetic expressions bash evaluates.
    sinks: Vec<String>,
    /// `NAME=value` operands of declaration builtins.
    declared: Vec<(String, String)>,
    /// A substitution whose output reaches arithmetic and is not a number.
    opaque_output: Option<String>,
    positional: bool,
    /// The raw value words a variable is assigned (`None`: a value with no
    /// single word, such as a compound array).
    words: HashMap<String, Vec<Option<String>>>,
    /// The argument words of every `wait`.
    waits: Vec<Vec<String>>,
    /// The first word arithmetic evaluates that starts with an unquoted `~`.
    tilde_sink: Option<String>,
}

/// Variables bash always holds a number in, unless the line assigns them.
const NUMBER_VARIABLES: &[&str] = &[
    "PPID",
    "BASHPID",
    "UID",
    "EUID",
    "RANDOM",
    "SRANDOM",
    "SECONDS",
    "LINENO",
    "EPOCHSECONDS",
];

/// Refuse the dataflow bypasses of the module docs. `command` keeps
/// unquoted heredoc bodies (they expand) and has no comments.
pub(super) fn check_dataflow(command: &str) -> Result<(), ShellError> {
    let mut flow = Flow::default();
    process_command(command, &mut flow, 0);
    if let Some(body) = flow.opaque_output.take() {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Arithmetic evaluates the output of `{body}` again, and \
             an output that spells an array subscript (`a[$(cmd)]`) runs cmd, so it cannot \
             be checked. Only commands that print a number (wc -l, nproc, date +%s, …) may \
             feed arithmetic."
        )
        .into());
    }
    if let Some(word) = flow
        .waits
        .iter()
        .find_map(|args| flow.wait_option_word(args))
    {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] `wait` reads its options after expansion, and `{word}` \
             may expand to `-p NAME`, which assigns NAME and evaluates an array subscript in \
             it (`a[$(cmd)]` runs cmd), so it cannot be checked. Pass `wait` literal options \
             and job ids that are numbers: `$!`, `$((…))` or a variable this command assigns \
             from them (`pid=$!; wait \"$pid\"`)."
        )
        .into());
    }
    if let Some(word) = flow.tilde_sink.take() {
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Arithmetic evaluates `{word}` after tilde expansion, \
             which reads $HOME, $PWD, $OLDPWD or the directory stack, and a value that \
             spells an array subscript (`a[$(cmd)]`) runs cmd, so it cannot be checked. \
             Use literal numbers or values computed by arithmetic."
        )
        .into());
    }
    let tainted = flow.tainted();
    for expr in &flow.sinks {
        let ArithmeticRefs {
            names: refs,
            outputs,
            built,
        } = arithmetic_refs(expr);
        if let Some(expansion) = built {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] The arithmetic `{expr}` uses `{expansion}`, which \
                 builds text (a default, a pattern removal, a substring, …). Arithmetic \
                 evaluates that text again, and text that spells an array subscript \
                 (`a[$(cmd)]`) runs cmd, so it cannot be checked. Use `${{NAME}}`, \
                 `${{#NAME}}` or `${{NAME[number]}}`, or assign the value to a variable first."
            )
            .into());
        }
        if let Some(body) = outputs.into_iter().find(|b| !prints_number(b)) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] Arithmetic evaluates the output of `{body}` again, \
                 and an output that spells an array subscript (`a[$(cmd)]`) runs cmd, so it \
                 cannot be checked. Only commands that print a number (wc -l, nproc, \
                 date +%s, …) may feed arithmetic."
            )
            .into());
        }
        if let Some(name) = refs.iter().find(|r| tainted.contains(*r)) {
            return Err(tainted_in_arithmetic(name, expr));
        }
    }
    for (target, value) in &flow.declared {
        let array = KNOWN_ARRAYS.contains(&target.as_str()) || flow.arrays.contains(target);
        if array && value.contains(['$', '`']) {
            return Err(format!(
                "[BLOCKED — DO NOT RETRY] `{target}={value}`: {target} is an array, and a \
                 declaration builtin re-parses an expanded value of the form `(…)` as a \
                 compound assignment, which runs any `$(cmd)` in it. Assign the array \
                 elements literally."
            )
            .into());
        }
    }
    Ok(())
}

fn tainted_in_arithmetic(name: &str, expr: &str) -> ShellError {
    let what = if name == POSITIONAL {
        "the positional parameters".to_string()
    } else {
        format!("`{name}`")
    };
    format!(
        "[BLOCKED — DO NOT RETRY] The arithmetic `{expr}` reads {what}, whose value this \
         command takes from text arithmetic would expand (an array subscript with `$(…)`, \
         input, a glob, a command's output). Bash evaluates that text again, which runs \
         any command in it, so it cannot be checked. Use literal numbers or values \
         computed by arithmetic."
    )
    .into()
}

impl Flow {
    /// Record the command word `word` that arithmetic evaluates. Bash
    /// tilde-expands it first: `~-` is `$OLDPWD`, `~+` `$PWD`, `~` `$HOME`,
    /// `~N` a directory stack entry, which the line may set to `a[$(cmd)]`.
    /// A word shaped like an assignment is expanded after its `=` and every
    /// `:` too (`let x=~-`).
    fn push_sink_word(&mut self, word: &str) {
        let tilde = word.starts_with('~')
            || assignment_parts(word).is_some_and(|(_, value)| expands_tilde(value));
        if tilde && self.tilde_sink.is_none() {
            self.tilde_sink = Some(word.to_string());
        }
        self.sinks.push(expression_text(word));
    }

    fn push_value(&mut self, name: &str, value: Value) {
        self.values.entry(name.to_string()).or_default().push(value);
    }

    fn push_word(&mut self, name: &str, word: Option<&str>) {
        self.words
            .entry(name.to_string())
            .or_default()
            .push(word.map(str::to_string));
    }

    /// The first word of `wait` `args` that is built at run time where an
    /// option may stand and is not a number. Like getopt, `wait` reads
    /// options up to the first other word (a job id) or `--`.
    fn wait_option_word<'a>(&self, args: &'a [String]) -> Option<&'a str> {
        let mut words = args.iter();
        while let Some(word) = words.next() {
            // `{-pBASH_ENV,}`, `-{p,}BASH_ENV`, or `-?BASH_ENV` with a file
            // `-pBASH_ENV`: bash expands braces and globs before `wait`
            // reads its options.
            if may_become_option(word) {
                return Some(word.as_str());
            }
            if word_pieces(word, false)
                .iter()
                .any(|p| matches!(p, Piece::Exp(_)))
            {
                return (!self.numeric_word(word, &mut HashSet::new())).then_some(word.as_str());
            }
            let text = literal(word);
            let cluster = text.strip_prefix('-').filter(|c| !c.is_empty())?;
            if cluster == "-" {
                return None;
            }
            // `-p NAME`: the name is the next word, checked by `side_doors`.
            if cluster.find('p').is_some_and(|at| at + 1 == cluster.len()) {
                words.next();
            }
        }
        None
    }

    /// Whether `word` expands to numbers (and blanks) only. `seen` holds the
    /// variables being checked: a value that reads its own variable
    /// (`pids="$pids $!"`) adds only numbers to it.
    fn numeric_word(&self, word: &str, seen: &mut HashSet<String>) -> bool {
        let pieces = word_pieces(word, false);
        !pieces.is_empty()
            && pieces.iter().all(|piece| match piece {
                Piece::Lit(c) => c.is_ascii_digit() || *c == ' ' || *c == '\t',
                Piece::Exp(e) => self.numeric_expansion(e, seen),
            })
    }

    fn numeric_expansion(&self, e: &str, seen: &mut HashSet<String>) -> bool {
        if is_arithmetic(e) {
            return true;
        }
        if let Some(body) = e.strip_prefix("$(").or_else(|| e.strip_prefix('`')) {
            let body = body
                .strip_suffix(')')
                .or_else(|| body.strip_suffix('`'))
                .unwrap_or(body);
            return prints_number(body);
        }
        let name = match e.strip_prefix("${") {
            Some(inner) => {
                let inner = inner.strip_suffix('}').unwrap_or(inner);
                // `${#NAME}`, `${#a[@]}`, `${##}`: a length. `${#:+word}`,
                // `${#/0/word}` are `$#` with an operator: text.
                if let Some(param) = inner.strip_prefix('#').filter(|p| !p.is_empty()) {
                    return is_length_parameter(param);
                }
                inner
            }
            None => &e[1..],
        };
        match name {
            "!" | "$" | "?" | "#" => true,
            n if is_identifier(n) => self.numeric_variable(n, seen),
            _ => false,
        }
    }

    fn numeric_variable(&self, name: &str, seen: &mut HashSet<String>) -> bool {
        if self.opaque.contains(name) || ALWAYS_TAINTED.contains(&name) {
            return false;
        }
        let Some(words) = self.words.get(name) else {
            return NUMBER_VARIABLES.contains(&name);
        };
        if !seen.insert(name.to_string()) {
            return true;
        }
        words
            .iter()
            .all(|w| w.as_deref().is_some_and(|w| self.numeric_word(w, seen)))
    }

    fn tainted(&self) -> HashSet<String> {
        let mut tainted: HashSet<String> = self.opaque.clone();
        tainted.extend(ALWAYS_TAINTED.iter().map(|s| (*s).to_string()));
        if self.positional {
            tainted.insert(POSITIONAL.to_string());
        }
        for (name, values) in &self.values {
            if values.iter().any(|v| matches!(v, Value::Unsafe)) {
                tainted.insert(name.clone());
            }
        }
        loop {
            let before = tainted.len();
            for (name, values) in &self.values {
                if tainted.contains(name) {
                    continue;
                }
                let reads_tainted = values.iter().any(|v| match v {
                    Value::Refs(refs) => refs.iter().any(|r| tainted.contains(r)),
                    Value::Unsafe => true,
                });
                if reads_tainted {
                    tainted.insert(name.clone());
                }
            }
            if tainted.len() == before {
                return tainted;
            }
        }
    }
}

fn process_command(text: &str, flow: &mut Flow, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let stripped = strip_for_analysis(text, false, true);
    for leaf in expand_to_leaf_segments(&stripped).unwrap_or_default() {
        process_statement(&leaf, flow, depth);
    }
    loop_headers(&stripped, flow);
    conditional_operands(&stripped, flow);
    scan_parameters(&stripped, false, flow, depth);
    let lexed = lex(text);
    for heredoc in lexed.heredocs.iter().filter(|h| !h.quoted) {
        if let Some(body) = text.get(heredoc.body.clone()) {
            scan_parameters(body, true, flow, depth);
        }
    }
    for expansion in &lexed.expansions {
        if expansion.arithmetic {
            flow.sinks.push(expansion.body.to_string());
            scan_parameters(&expansion.body, false, flow, depth);
        }
        process_command(&expansion.body, flow, depth + 1);
    }
}

/// A word that redirects (`>f`, `2>&1`, `<<<`), and whether its target is
/// the next word.
fn redirection(word: &str) -> Option<bool> {
    let rest = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
    let op_len = rest
        .bytes()
        .take_while(|b| matches!(b, b'<' | b'>' | b'&' | b'|'))
        .count();
    (op_len > 0 && !rest.starts_with("<(") && !rest.starts_with(">("))
        .then_some(rest.len() == op_len)
}

/// The words of a statement without its redirections.
fn statement_words(segment: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut skip_next = false;
    for word in raw_words(segment) {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        match redirection(word) {
            Some(target_follows) => skip_next = target_follows,
            None => words.push(word),
        }
    }
    words
}

/// The text a word stands for with quotes removed and expansions kept as
/// written, for reading it as an arithmetic expression.
fn expression_text(word: &str) -> String {
    word_pieces(word, false)
        .into_iter()
        .map(|p| match p {
            Piece::Lit(c) => c.to_string(),
            Piece::Exp(e) => e.to_string(),
        })
        .collect()
}

fn literal(word: &str) -> String {
    word_pieces(word, false)
        .into_iter()
        .map(|p| match p {
            Piece::Lit(c) => c,
            Piece::Exp(_) => '\u{1}',
        })
        .collect()
}

/// Whether `param`, the text after `${#`, is a bare parameter whose length
/// `${#…}` gives: a name, a digit string or one special character,
/// optionally with one `[…]` subscript, and nothing after it.
fn is_length_parameter(param: &str) -> bool {
    let name_len = param
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let (name, rest) = if name_len == 0 {
        param.split_at(param.len().min(1))
    } else {
        param.split_at(name_len)
    };
    let bare = is_identifier(name)
        || (!name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        || matches!(name, "@" | "*" | "#" | "?" | "$" | "!" | "-");
    let rest_ok = rest.is_empty()
        || (is_identifier(name)
            && rest.starts_with('[')
            && matching_close(rest, 0, b'[', b']') == rest.len()
            && rest.ends_with(']'));
    bare && rest_ok
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn process_statement(segment: &str, flow: &mut Flow, depth: usize) {
    if detect_function_def(segment).is_some() {
        // A function's positional parameters are its caller's arguments.
        flow.positional = true;
        if let Some(body) = function_body(segment) {
            process_command(body, flow, depth + 1);
        }
        return;
    }
    let words = statement_words(segment);
    let mut at = 0;
    while let Some(word) = words.get(at) {
        if !assignment(word, flow) {
            break;
        }
        at += 1;
    }
    let Some(command) = words.get(at) else {
        return;
    };
    let command = literal(command);
    let args = &words[at + 1..];
    let wrapped = matches!(command.as_str(), "builtin" | "command")
        .then(|| {
            args.iter()
                .map(|a| literal(a))
                .find(|a| !a.starts_with('-'))
        })
        .flatten();
    if ["cd", "pushd", "popd"]
        .iter()
        .any(|c| command == *c || wrapped.as_deref() == Some(*c))
    {
        flow.opaque
            .extend(DIRECTORY_VARIABLES.iter().map(|s| (*s).to_string()));
    }
    let wait_args = if command == "wait" {
        Some(args)
    } else if wrapped.as_deref() == Some("wait") {
        args.iter()
            .position(|a| literal(a) == "wait")
            .map(|at| &args[at + 1..])
    } else {
        None
    };
    if let Some(wait_args) = wait_args {
        flow.waits
            .push(wait_args.iter().map(|a| (*a).to_string()).collect());
    }
    match command.as_str() {
        c if DECLARATION_BUILTINS.contains(&c) => {
            for arg in args {
                if let Some((name, value)) = assignment_parts(arg) {
                    flow.declared.push((name.to_string(), value.to_string()));
                    assignment(arg, flow);
                }
            }
        }
        "read" => {
            let mut named = false;
            let mut iter = args.iter().map(|a| literal(a));
            while let Some(arg) = iter.next() {
                if arg == "-a" {
                    if let Some(name) = iter.next() {
                        flow.arrays.insert(name.clone());
                        flow.opaque.insert(name);
                        named = true;
                    }
                } else if ["-p", "-d", "-t", "-n", "-N", "-u", "-i"].contains(&arg.as_str()) {
                    iter.next();
                } else if !arg.starts_with('-') {
                    flow.opaque.insert(arg);
                    named = true;
                }
            }
            if !named {
                flow.opaque.insert("REPLY".to_string());
            }
        }
        "mapfile" | "readarray" => {
            let mut iter = args.iter().map(|a| literal(a));
            let mut name = "MAPFILE".to_string();
            while let Some(arg) = iter.next() {
                if ["-d", "-n", "-O", "-s", "-u", "-C", "-c"].contains(&arg.as_str()) {
                    iter.next();
                } else if !arg.starts_with('-') {
                    name = arg;
                    break;
                }
            }
            flow.arrays.insert(name.clone());
            flow.opaque.insert(name);
        }
        // Options are read after expansion (`o=-v; printf $o X …`). A name
        // hidden in a splitting word is refused by `side_doors`.
        "printf" => {
            if let PrintfTarget::Variable(name) = printf_target(args) {
                flow.opaque.insert(name);
            }
        }
        "getopts" => {
            if let Some(name) = args.get(1) {
                flow.opaque.insert(literal(name));
            }
            flow.opaque.insert("OPTARG".to_string());
        }
        // `[[ … ]]` is read whole by `conditional_operands`.
        "test" | "[" => {
            for (i, arg) in args.iter().enumerate() {
                if literal(arg) == "-v" {
                    if let Some(after) = args.get(i + 1) {
                        flow.push_sink_word(after);
                    }
                }
            }
        }
        // `let` evaluates its operands arithmetically, and zsh the others.
        "let" | "exit" | "return" | "shift" | "break" | "continue" => {
            for arg in args {
                flow.push_sink_word(arg);
            }
        }
        "set"
            if args
                .iter()
                .any(|a| a.starts_with("--") || !a.starts_with(['-', '+'])) =>
        {
            flow.positional = true;
        }
        _ => {}
    }
}

/// `NAME=value`, `NAME+=value` or `NAME[sub]=value` split into the name
/// (with any subscript) and the raw value.
fn assignment_parts(word: &str) -> Option<(&str, &str)> {
    let name_len = word
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    if name_len == 0 || word.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let mut end = name_len;
    if word.as_bytes().get(end) == Some(&b'[') {
        end = matching_close(word, end, b'[', b']');
    }
    let rest = &word[end..];
    let value = rest.strip_prefix("+=").or_else(|| rest.strip_prefix('='))?;
    Some((&word[..end], value))
}

/// Record the assignment `word`, if it is one.
fn assignment(word: &str, flow: &mut Flow) -> bool {
    let Some((target, value)) = assignment_parts(word) else {
        return false;
    };
    let (name, subscript) = match target.split_once('[') {
        Some((name, sub)) => (name, Some(sub.strip_suffix(']').unwrap_or(sub))),
        None => (target, None),
    };
    if let Some(sub) = subscript {
        flow.sinks.push(expression_text(sub));
        flow.arrays.insert(name.to_string());
    }
    if let Some(inner) = value
        .strip_prefix('(')
        .map(|v| v.strip_suffix(')').unwrap_or(v))
    {
        flow.arrays.insert(name.to_string());
        for element in raw_words(inner) {
            let element = match element.strip_prefix('[') {
                Some(keyed) => {
                    let close = matching_close(element, 0, b'[', b']');
                    flow.sinks
                        .push(expression_text(&keyed[..close.saturating_sub(2)]));
                    element[close..]
                        .strip_prefix('=')
                        .unwrap_or(&element[close..])
                }
                None => element,
            };
            // Bash globs the elements: a file named `a[$(cmd)]` becomes one.
            let class = if unquoted_glob(element) {
                Value::Unsafe
            } else {
                classify(element)
            };
            flow.push_value(name, class);
        }
        flow.push_word(name, None);
        return true;
    }
    let class = classify(value);
    flow.push_value(name, class);
    flow.push_word(name, Some(value));
    true
}

/// Whether `word` holds an unquoted glob character (`*`, `?`, `[`), an
/// extglob group (`@(…)`, `!(…)`, `+(…)`) or an unquoted expansion whose
/// value may hold one (`Y='a*'; X=($Y)`), so pathname expansion may replace
/// it with file names. Only an expansion known to give a number is safe.
fn unquoted_glob(word: &str) -> bool {
    let bytes = word.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if !in_single => i += 1,
            _ if in_single || in_double => {}
            b'$' => {
                if let Some(end) = expansion_end(word, i) {
                    if !gives_number(&word[i..end]) {
                        return true;
                    }
                    i = end;
                    continue;
                }
            }
            b'`' => {
                let end = backtick_end(word, i + 1);
                if !gives_number(&word[i..end]) {
                    return true;
                }
                i = end;
                continue;
            }
            b'*' | b'?' | b'[' => return true,
            b'@' | b'!' | b'+' if bytes.get(i + 1) == Some(&b'(') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Characters a value may hold for arithmetic to evaluate it without
/// expanding anything: numbers, names and operators, no `[`, `$` or quote.
fn arithmetic_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || " \t_+-*/%<>=!&|^~?:,.()#".contains(c)
}

/// Whether arithmetic evaluating `raw` (an assigned value) could expand
/// something, or which names it reads.
fn classify(raw: &str) -> Value {
    if expands_tilde(raw) {
        return Value::Unsafe;
    }
    let mut refs = Vec::new();
    let mut lit = String::new();
    for piece in word_pieces(raw, false) {
        match piece {
            Piece::Lit(c) if arithmetic_safe(c) => lit.push(c),
            Piece::Lit(_) => return Value::Unsafe,
            Piece::Exp(e) => {
                lit.push(' ');
                match expansion_refs(e) {
                    Some(names) => refs.extend(names),
                    None => return Value::Unsafe,
                }
            }
        }
    }
    refs.extend(identifiers(&lit));
    Value::Refs(refs)
}

/// Whether bash tilde-expands `raw`, an assigned value: an unquoted `~` at
/// its start or after an unquoted `:` (`x=0?2:~-`) reads `$HOME`, `$PWD`,
/// `$OLDPWD` or the directory stack, whose value the line may set.
fn expands_tilde(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut word_start = true;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        let starts_here = std::mem::replace(&mut word_start, false);
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if !in_single => i += 1,
            _ if in_single || in_double => {}
            b'~' if starts_here => return true,
            b':' => word_start = true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Whether expansion `e` gives a number whatever the line holds
/// (`$((…))`, `$?`, `$(wc -l …)`), so it holds no glob character.
fn gives_number(e: &str) -> bool {
    expansion_refs(e).is_some_and(|names| names.is_empty())
}

/// The names an expansion's result depends on, or `None` when its result is
/// text the line does not control (a command's output other than a number,
/// a pattern substitution, `$_`).
fn expansion_refs(e: &str) -> Option<Vec<String>> {
    if is_arithmetic(e) {
        return Some(Vec::new());
    }
    if let Some(body) = e.strip_prefix("$(").or_else(|| e.strip_prefix('`')) {
        let body = body
            .strip_suffix(')')
            .or_else(|| body.strip_suffix('`'))
            .unwrap_or(body);
        return prints_number(body).then(Vec::new);
    }
    if let Some(inner) = e.strip_prefix("${") {
        let inner = inner.strip_suffix('}').unwrap_or(inner);
        let inner = inner.strip_prefix('#').unwrap_or(inner);
        let name_len = inner
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        let (name, rest) = if name_len == 0 {
            inner.split_at(inner.len().min(1))
        } else {
            inner.split_at(name_len)
        };
        let rest = match rest.strip_prefix('[') {
            Some(_) => &rest[matching_close(rest, 0, b'[', b']')..],
            None => rest,
        };
        let default = rest.strip_prefix(":-").or_else(|| rest.strip_prefix('-'));
        let mut names = parameter_ref(name)?;
        match default {
            _ if rest.is_empty() => {}
            Some(word) if word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                names.extend(identifiers(word));
            }
            _ => return None,
        }
        return Some(names);
    }
    parameter_ref(&e[1..])
}

/// The name a `$NAME` / `${NAME}` reads, `POSITIONAL` for `$1`, `$@`, `$*`.
fn parameter_ref(name: &str) -> Option<Vec<String>> {
    match name {
        "_" => None,
        "?" | "#" | "$" | "!" | "-" => Some(Vec::new()),
        "@" | "*" => Some(vec![POSITIONAL.to_string()]),
        n if n.bytes().all(|b| b.is_ascii_digit()) && !n.is_empty() => {
            Some(vec![POSITIONAL.to_string()])
        }
        n if is_identifier(n) => Some(vec![n.to_string()]),
        _ => None,
    }
}

/// Every name in an arithmetic text: a word starting with a digit is a
/// number (`0x1f`, `16#ff`).
fn identifiers(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '#'))
        .filter(|w| !w.is_empty() && !w.as_bytes()[0].is_ascii_digit())
        .flat_map(|w| w.split('#'))
        .filter(|w| is_identifier(w))
        .map(str::to_string)
        .collect()
}

/// What an arithmetic expression depends on.
struct ArithmeticRefs {
    /// The names it reads.
    names: Vec<String>,
    /// The bodies of the command substitutions in it.
    outputs: Vec<String>,
    /// The first `${…}` in it that builds text (see `builds_text`).
    built: Option<String>,
}

fn arithmetic_refs(expr: &str) -> ArithmeticRefs {
    let mut outputs = Vec::new();
    let mut built = None;
    let mut blanked = String::with_capacity(expr.len());
    let mut positional = false;
    let mut i = 0;
    let bytes = expr.as_bytes();
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'$' && bytes.get(i + 1) == Some(&b'(') && bytes.get(i + 2) != Some(&b'(') {
            let end = matching_close(expr, i + 1, b'(', b')');
            outputs.push(expr[i + 2..end.saturating_sub(1).max(i + 2)].to_string());
            blanked.push(' ');
            i = end;
        } else if b == b'`' {
            let end = expr[i + 1..].find('`').map_or(expr.len(), |at| i + 2 + at);
            outputs.push(expr[i + 1..end.saturating_sub(1).max(i + 1)].to_string());
            blanked.push(' ');
            i = end;
        } else if b == b'$' && bytes.get(i + 1) == Some(&b'{') {
            let end = matching_close(expr, i + 1, b'{', b'}');
            let expansion = &expr[i..end];
            if builds_text(expansion) {
                built.get_or_insert_with(|| expansion.to_string());
                blanked.push(' ');
            } else {
                blanked.push_str(expansion);
            }
            i = end;
        } else if b == b'$'
            && bytes
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_digit() || matches!(n, b'@' | b'*'))
        {
            positional = true;
            blanked.push(' ');
            i += 2;
        } else {
            let c = expr[i..].chars().next().unwrap_or(' ');
            blanked.push(c);
            i += c.len_utf8();
        }
    }
    let mut names = identifiers(&blanked);
    if positional {
        names.push(POSITIONAL.to_string());
    }
    ArithmeticRefs {
        names,
        outputs,
        built,
    }
}

/// Whether the command line `body` prints only a number: its last pipeline
/// stage is one of a few counting commands.
fn prints_number(body: &str) -> bool {
    let segments = segments_with_separators(body);
    let Some(((last, _), before)) = segments.split_last() else {
        return false;
    };
    if before.iter().any(|(_, sep)| *sep != Some(Separator::Pipe)) {
        return false;
    }
    let tokens = extract_command_tokens_from_segment(last);
    let mut words: Vec<&str> = Vec::new();
    let mut skip_next = false;
    for token in &tokens {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        match redirection(token) {
            Some(target_follows) => skip_next = target_follows,
            None => words.push(token),
        }
    }
    let Some((program, args)) = words.split_first() else {
        return false;
    };
    let numeric = |a: &&str| a.parse::<f64>().is_ok();
    match *program {
        "nproc" | "getconf" => true,
        // Process ids (`wait $(jobs -p)`).
        "jobs" => !args.is_empty() && args.iter().all(|a| matches!(*a, "-p" | "-pr" | "-ps")),
        "id" => args
            .iter()
            .all(|a| matches!(*a, "-u" | "-g" | "-r" | "-ur" | "-gr")),
        "date" => args.iter().all(|a| {
            *a == "-u"
                || a.strip_prefix('+')
                    .is_some_and(|f| f.chars().all(|c| c == '%' || c.is_ascii_alphanumeric()))
        }),
        "wc" => args.iter().all(|a| a.starts_with('-')),
        "seq" => args.iter().all(numeric),
        "grep" | "rg" => {
            let options: Vec<&&str> = args.iter().filter(|a| a.starts_with('-')).collect();
            let counts = options
                .iter()
                .any(|o| !o.starts_with("--") && o.contains('c') || **o == "--count");
            let recursive = options
                .iter()
                .any(|o| !o.starts_with("--") && o.contains(['r', 'R']));
            counts && !recursive && args.len() - options.len() <= 2
        }
        "git" => args.first() == Some(&"rev-list") && args.contains(&"--count"),
        _ => false,
    }
}

/// `for NAME in WORDS` and `select NAME in WORDS` headers, which the leaf
/// expansion drops: the loop variable takes each word.
fn loop_headers(text: &str, flow: &mut Flow) {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?:^|[\s;&|(){}])(for|select)\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:(in)(?:\s([^;\n]*))?)?",
        )
        .expect("loop header pattern")
    });
    let blanked = blank_quoted(text);
    for caps in HEADER.captures_iter(&blanked) {
        let (Some(keyword), Some(name)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let name = name.as_str();
        let words = caps
            .get(4)
            .and_then(|m| text.get(m.range()))
            .unwrap_or_default();
        if keyword.as_str() == "select" || caps.get(3).is_none() {
            flow.opaque.insert(name.to_string());
            continue;
        }
        for word in raw_words(words) {
            if word == "do" {
                break;
            }
            let glob = word.contains(['*', '?', '[']) || unquoted_glob(word);
            let class = if glob { Value::Unsafe } else { classify(word) };
            flow.push_value(name, class);
            flow.push_word(name, (!glob).then_some(word));
        }
    }
}

/// The operands `[[ … ]]` evaluates arithmetically: both sides of `-eq`,
/// `-ne`, `-lt`, `-le`, `-gt` and `-ge`, and the name after `-v`.
///
/// Each conditional is read whole, in `[[` grammar. The leaf split cuts
/// `[[ 0 -eq 1 || ~- -eq 0 ]]` at `||`, so its second half no longer starts
/// with `[[`, and the word splitter keeps a group like `( X -eq 0 )` as one
/// word. Bash still evaluates the operands in both.
///
/// A `]]` ends the conditional only outside every group: after `=~` bash
/// reads `( ]] )` as regex text, and in a plain group it is a syntax error,
/// so `[[ x =~ ( ]] ) || v -eq 0 ]]` still compares `v`.
fn conditional_operands(text: &str, flow: &mut Flow) {
    let words = conditional_words(text);
    let is_operator = |w: &str| matches!(w, "(" | ")" | "&&" | "||" | "!");
    let mut start = 0;
    while let Some(open) = words[start..].iter().position(|w| *w == "[[") {
        let open = start + open;
        let mut depth = 0usize;
        let close = words[open..]
            .iter()
            .position(|w| {
                match *w {
                    "(" => depth += 1,
                    ")" => depth = depth.saturating_sub(1),
                    _ => {}
                }
                depth == 0 && *w == "]]"
            })
            .map_or(words.len(), |at| open + at);
        let span = &words[open + 1..close];
        for (i, word) in span.iter().enumerate() {
            let op = literal(word);
            let compares = matches!(op.as_str(), "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge");
            let before = i.checked_sub(1).and_then(|j| span.get(j));
            if let Some(before) = before.filter(|w| compares && !is_operator(w)) {
                flow.push_sink_word(before);
            }
            if let Some(after) = span.get(i + 1) {
                if (compares || op == "-v") && !is_operator(after) {
                    flow.push_sink_word(after);
                }
            }
        }
        start = (close + 1).min(words.len());
    }
}

/// The words of `text` in `[[` grammar. Unquoted whitespace ends a word, and
/// `(`, `)`, `&&`, `||`, `&`, `|`, `;` and a newline are words of their own
/// even without spaces around them. Quotes, escapes, expansions and command
/// substitutions stay inside the word that holds them.
fn conditional_words(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    let mut in_double = false;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if in_double {
            i = match b {
                b'\\' => i + 2,
                b'"' => {
                    in_double = false;
                    i + 1
                }
                b'$' => expansion_end(text, i).unwrap_or(i + 1).max(i + 1),
                b'`' => backtick_end(text, i + 1),
                _ => i + 1,
            }
            .min(len);
            continue;
        }
        let operator = match b {
            b'&' | b'|' if bytes.get(i + 1) == Some(&b) => 2,
            b'(' | b')' | b';' | b'\n' | b'&' | b'|' => 1,
            _ => 0,
        };
        if operator > 0 || b.is_ascii_whitespace() {
            if let Some(from) = start.take() {
                words.push(&text[from..i]);
            }
            if operator > 0 {
                words.push(&text[i..i + operator]);
            }
            i += operator.max(1);
            continue;
        }
        start.get_or_insert(i);
        i = match b {
            b'\\' => i + 2,
            b'\'' => text[i + 1..].find('\'').map_or(len, |at| i + 2 + at),
            b'"' => {
                in_double = true;
                i + 1
            }
            b'$' => expansion_end(text, i).unwrap_or(i + 1).max(i + 1),
            b'`' => backtick_end(text, i + 1),
            _ => i + 1,
        }
        .min(len);
    }
    if let Some(from) = start {
        words.push(&text[from..]);
    }
    words
}

/// `text` with every quoted or escaped character replaced by a byte that is
/// no word character, offsets kept, so keywords are found only where the
/// shell reads them.
fn blank_quoted(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        let quoted = in_single || in_double;
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if !in_single => {
                out[i] = 1;
                if i + 1 < bytes.len() {
                    // Blank every byte of the escaped character.
                    let len = text[i + 1..].chars().next().map_or(1, char::len_utf8);
                    for slot in &mut out[i + 1..i + 1 + len] {
                        *slot = 1;
                    }
                    i += len;
                }
            }
            _ if quoted => out[i] = 1,
            _ => {}
        }
        i += 1;
    }
    // Only whole characters were replaced by ASCII bytes.
    String::from_utf8(out).unwrap_or_default()
}

/// Every `${…}` and `$[…]` in `text` (not in single quotes): subscripts and
/// substring offsets are arithmetic, `${NAME:=…}` assigns.
fn scan_parameters(text: &str, heredoc: bool, flow: &mut Flow, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double && !heredoc => in_single = !in_single,
            b'"' if !in_single && !heredoc => in_double = !in_double,
            b'\\' if !in_single => i += 1,
            b'$' if !in_single => {
                if let Some(end) = expansion_end(text, i) {
                    let exp = &text[i..end];
                    if let Some(inner) = exp.strip_prefix("${") {
                        let inner = inner.strip_suffix('}').unwrap_or(inner);
                        parameter(inner, flow);
                        scan_parameters(inner, false, flow, depth + 1);
                    } else if let Some(inner) = exp.strip_prefix("$[") {
                        let inner = inner.strip_suffix(']').unwrap_or(inner);
                        flow.sinks.push(inner.to_string());
                        scan_parameters(inner, false, flow, depth + 1);
                    }
                    i = end;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// The arithmetic and assignments of one `${…}` body.
fn parameter(inner: &str, flow: &mut Flow) {
    let inner = inner.strip_prefix(['#', '!']).unwrap_or(inner);
    let name_len = inner
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let (name, mut rest) = inner.split_at(name_len);
    if rest.starts_with('[') {
        let close = matching_close(rest, 0, b'[', b']');
        let sub = &rest[1..close.saturating_sub(1).max(1)];
        if sub != "@" && sub != "*" {
            flow.sinks.push(sub.to_string());
        }
        rest = &rest[close..];
    }
    if rest.starts_with(":=") || rest.starts_with('=') {
        flow.opaque.insert(name.to_string());
    } else if let Some(offsets) = rest.strip_prefix(':') {
        if !offsets.starts_with(['-', '=', '?', '+']) {
            flow.sinks.extend(offsets.split(':').map(str::to_string));
        }
    }
}
