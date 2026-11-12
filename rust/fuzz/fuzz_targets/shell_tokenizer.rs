// SPDX-License-Identifier: Apache-2.0
//! Fuzz the shell tokenizer, the command splitter, the lexer and the
//! allowlist gate (W3-08). Crashes are panics, stack overflows and a broken
//! tokenizer round trip; `tests/proptest_tokenizer.rs` in
//! lean-ctx-shell-policy states the same properties for `cargo test`.
#![no_main]

use std::sync::Once;

use lean_ctx::core::shell_allowlist::{
    extract_all_commands, extract_base_command, lexer, quote_aware_token_end,
    segments_with_separators, sequence_statements, shell_tokenize, skip_env_assignments,
    split_on_operators,
};
use libfuzzer_sys::fuzz_target;

static ENV: Once = Once::new();

fn single_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

fuzz_target!(|data: &[u8]| {
    ENV.call_once(|| {
        // SAFETY: set once, before the gate first reads the environment;
        // libFuzzer runs the target on one thread.
        unsafe {
            std::env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
            std::env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git,ls,echo,cat,wc");
        }
    });
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };

    let tokens = shell_tokenize(input);
    let _ = split_on_operators(input);
    let _ = segments_with_separators(input);
    let _ = sequence_statements(input);
    let _ = extract_all_commands(input);
    let _ = extract_base_command(input);
    let _ = skip_env_assignments(input);
    let _ = lexer::lex(input);
    let end = quote_aware_token_end(input);
    assert!(input.is_char_boundary(end), "token end {end} splits a character");

    // Tokens are never empty, and re-quoting them reads back the same words.
    assert!(tokens.iter().all(|t| !t.is_empty()));
    let requoted = tokens.iter().map(|t| single_quote(t)).collect::<Vec<_>>().join(" ");
    assert_eq!(shell_tokenize(&requoted), tokens, "requoted: {requoted:?}");

    let _ = lean_ctx_shell_policy::check_shell_allowlist(input);
});
