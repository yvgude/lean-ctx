// SPDX-License-Identifier: Apache-2.0
//! Property tests for the shell tokenizer, the command splitter and the
//! allowlist decision (W3-08, deterministic checkers).
//!
//! The example-based suites (`src/tests_tokenizer.rs`, `src/lexer_tests.rs`,
//! `tests/attack_scenarios.rs`) pin known inputs; these properties sweep the
//! input space around them:
//!
//! - every parser entry point is total: no panic on arbitrary input;
//! - `shell_tokenize` inverts three quoting schemes (single quotes, double
//!   quotes, backslash escapes), so `tokenize(join(words)) == words`;
//! - the allowlist decision does not depend on how much whitespace separates
//!   words and operators, nor on quoting plain arguments;
//! - quoting or escaping inside a program name, which the shell removes
//!   before it runs the program, never lets a program that is not listed
//!   through.
//!
//! `fuzz/` (cargo-fuzz, outside the workspace) drives the same entry points
//! with coverage guidance.

use std::sync::{Mutex, MutexGuard};

use lean_ctx::core::shell_allowlist::{
    extract_all_commands, extract_base_command, lexer, quote_aware_token_end,
    segments_with_separators, sequence_statements, shell_tokenize, skip_env_assignments,
    split_on_operators,
};
use lean_ctx_shell_policy::check_shell_allowlist;
use proptest::prelude::*;

/// Programs on the allowlist the gate properties run under.
const LISTED: &[&str] = &["git", "ls", "echo", "cat", "wc"];
/// Programs that are not on it.
const UNLISTED: &[&str] = &["curl", "rm", "wget", "nc", "python3", "sh"];

/// Serializes the environment the gate reads; every test in this binary that
/// calls into the gate holds it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct GateEnv {
    _lock: MutexGuard<'static, ()>,
}

impl GateEnv {
    /// Enforce mode with exactly [`LISTED`] on the allowlist, regardless of
    /// the developer's own runtime configuration.
    fn enforce() -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: every test in this binary that touches the environment or
        // the gate holds `ENV_LOCK`, so nothing reads it concurrently.
        unsafe {
            std::env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
            std::env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", LISTED.join(","));
            std::env::remove_var("LEAN_CTX_ALLOWLIST_WARN_ONLY");
        }
        Self { _lock: lock }
    }
}

fn allowed(command: &str) -> bool {
    check_shell_allowlist(command).is_ok()
}

/// Characters that steer the tokenizer, splitter and lexer.
const SHELL_ALPHABET: &[char] = &[
    'a', 'b', 'g', 'i', 't', ' ', '\t', '\n', '\'', '"', '\\', '$', '{', '}', '(', ')', '`', ';',
    '|', '&', '<', '>', '#', '=', '-', '*', '?', '~', '!', '\r', 'é', '\u{2019}', '\u{201C}',
];

/// Strings biased toward shell syntax, mixed with fully arbitrary ones.
fn shellish(max_len: usize) -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::collection::vec(prop::sample::select(SHELL_ALPHABET), 0..max_len)
            .prop_map(|chars| chars.into_iter().collect::<String>()),
        1 => any::<String>(),
    ]
}

/// A plain word: no quoting, no metacharacters, never an option.
fn plain_word() -> impl Strategy<Value = String> {
    "[a-z0-9][a-z0-9_.]{0,7}"
}

/// Any non-empty word, including whitespace, quotes and metacharacters.
fn any_word() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::collection::vec(prop::sample::select(SHELL_ALPHABET), 1..12)
            .prop_map(|chars| chars.into_iter().collect::<String>()),
        "\\PC{1,12}",
    ]
}

/// One run of blanks between words.
fn blanks() -> impl Strategy<Value = String> {
    "[ \t]{1,4}"
}

fn single_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

fn double_quote(word: &str) -> String {
    let mut out = String::from("\"");
    for c in word.chars() {
        if matches!(c, '$' | '`' | '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn backslash_escape(word: &str) -> String {
    word.chars().flat_map(|c| ['\\', c]).collect()
}

/// A simple command: a program and plain arguments.
fn simple_command() -> impl Strategy<Value = (String, Vec<String>)> {
    let program = prop::sample::select([LISTED, UNLISTED].concat()).prop_map(str::to_string);
    (program, prop::collection::vec(plain_word(), 0..4))
}

/// Up to three simple commands joined by operators.
fn pipeline() -> impl Strategy<Value = Vec<((String, Vec<String>), &'static str)>> {
    prop::collection::vec(
        (
            simple_command(),
            prop::sample::select(&[";", "|", "&&", "||"][..]),
        ),
        1..4,
    )
}

fn render(
    commands: &[((String, Vec<String>), &'static str)],
    mut gap: impl FnMut(usize) -> String,
    quote: impl Fn(&str) -> String,
) -> String {
    let mut out = String::new();
    let mut n = 0;
    for (i, ((program, args), op)) in commands.iter().enumerate() {
        out.push_str(program);
        for arg in args {
            out.push_str(&gap(n));
            n += 1;
            out.push_str(&quote(arg));
        }
        if i + 1 < commands.len() {
            out.push_str(&gap(n));
            n += 1;
            out.push_str(op);
            out.push_str(&gap(n));
            n += 1;
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The parsing entry points are total.
    #[test]
    fn parsers_never_panic(input in shellish(96)) {
        let _ = shell_tokenize(&input);
        let _ = split_on_operators(&input);
        let _ = segments_with_separators(&input);
        let _ = sequence_statements(&input);
        let _ = extract_all_commands(&input);
        let _ = extract_base_command(&input);
        let _ = lexer::lex(&input);
        let _ = skip_env_assignments(&input);
        let end = quote_aware_token_end(&input);
        prop_assert!(end <= input.len());
        prop_assert!(input.is_char_boundary(end), "token end {end} splits a character");
    }

    /// Single quotes protect every character.
    #[test]
    fn tokenize_inverts_single_quoting(words in prop::collection::vec(any_word(), 0..6)) {
        let line = words.iter().map(|w| single_quote(w)).collect::<Vec<_>>().join(" ");
        prop_assert_eq!(shell_tokenize(&line), words, "line: {:?}", line);
    }

    /// Double quotes protect every character once `$ ` \ "` are escaped.
    #[test]
    fn tokenize_inverts_double_quoting(words in prop::collection::vec(any_word(), 0..6)) {
        let line = words.iter().map(|w| double_quote(w)).collect::<Vec<_>>().join(" ");
        prop_assert_eq!(shell_tokenize(&line), words, "line: {:?}", line);
    }

    /// A backslash outside quotes takes the next character literally.
    #[test]
    fn tokenize_inverts_backslash_escaping(words in prop::collection::vec(any_word(), 0..6)) {
        let line = words.iter().map(|w| backslash_escape(w)).collect::<Vec<_>>().join(" ");
        prop_assert_eq!(shell_tokenize(&line), words, "line: {:?}", line);
    }

    /// Runs of blanks separate words like a single space does.
    #[test]
    fn tokenize_ignores_blank_runs(
        words in prop::collection::vec(plain_word(), 0..6),
        gaps in prop::collection::vec(blanks(), 7),
        lead in "[ \t]{0,3}",
        trail in "[ \t]{0,3}",
    ) {
        let mut line = lead;
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                line.push_str(&gaps[i]);
            }
            line.push_str(word);
        }
        line.push_str(&trail);
        prop_assert_eq!(shell_tokenize(&line), words);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// The gate is total, and refuses rather than crashes.
    #[test]
    fn gate_never_panics(input in shellish(96)) {
        let _env = GateEnv::enforce();
        let _ = check_shell_allowlist(&input);
    }

    /// Blank runs around words and operators do not change the decision.
    #[test]
    fn decision_ignores_blank_runs(
        commands in pipeline(),
        gaps in prop::collection::vec(blanks(), 24),
    ) {
        let _env = GateEnv::enforce();
        let canonical = render(&commands, |_| " ".to_string(), str::to_string);
        let spaced = render(&commands, |i| gaps[i % gaps.len()].clone(), str::to_string);
        prop_assert_eq!(allowed(&canonical), allowed(&spaced), "{:?} vs {:?}", canonical, spaced);
    }

    /// Quoting plain arguments does not change the decision.
    #[test]
    fn decision_ignores_argument_quoting(commands in pipeline(), style in 0u8..3) {
        let _env = GateEnv::enforce();
        let canonical = render(&commands, |_| " ".to_string(), str::to_string);
        let quoted = render(&commands, |_| " ".to_string(), |w| match style {
            0 => single_quote(w),
            1 => double_quote(w),
            _ => backslash_escape(w),
        });
        prop_assert_eq!(allowed(&canonical), allowed(&quoted), "{:?} vs {:?}", canonical, quoted);
    }

    /// A pipeline passes only when every program in it is listed.
    #[test]
    fn decision_requires_every_program_listed(commands in pipeline()) {
        let _env = GateEnv::enforce();
        let line = render(&commands, |_| " ".to_string(), str::to_string);
        let all_listed = commands.iter().all(|((program, _), _)| LISTED.contains(&program.as_str()));
        if !all_listed {
            prop_assert!(!allowed(&line), "unlisted program passed: {:?}", line);
        }
    }

    /// The shell removes quotes and backslashes from a program name before it
    /// runs it; spelling an unlisted program that way must not get it past.
    #[test]
    fn quoted_program_name_never_unlocks(
        program in prop::sample::select(UNLISTED),
        args in prop::collection::vec(plain_word(), 0..3),
        split in 0usize..8,
        style in 0u8..4,
    ) {
        let _env = GateEnv::enforce();
        let at = split % program.len();
        let (head, tail) = program.split_at(at);
        let spelled = match style {
            0 => single_quote(program),
            1 => double_quote(program),
            2 => format!("{head}''{tail}"),
            _ => format!("{head}\\{tail}"),
        };
        let line = std::iter::once(spelled).chain(args).collect::<Vec<_>>().join(" ");
        prop_assert!(!allowed(&line), "unlisted program passed as {:?}", line);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    /// Deep nesting is bounded: no stack overflow, and an unlisted program
    /// nested at any depth (also past the lexer's bound) is refused.
    ///
    /// Every opener leaves `curl` in command position: `$(echo x; curl x)`
    /// runs curl, whereas `$(echo curl x)` would only print the word.
    #[test]
    fn deep_nesting_is_refused_not_crashed(
        (open, close) in prop::sample::select(
            &[("$(", ")"), ("$( ", " )"), ("\"$(", ")\""), ("<(", ")"), ("$(echo x; ", ")")][..],
        ),
        depth in 1usize..400,
    ) {
        let _env = GateEnv::enforce();
        let line = format!("echo {}curl x{}", open.repeat(depth), close.repeat(depth));
        let _ = shell_tokenize(&line);
        let _ = extract_all_commands(&line);
        let _ = lexer::lex(&line);
        prop_assert!(!allowed(&line), "nested unlisted program passed: depth {depth} {open:?}");
    }
}
