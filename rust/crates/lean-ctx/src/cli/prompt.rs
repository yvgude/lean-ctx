// SPDX-License-Identifier: Apache-2.0
//! Shared interactive-confirmation helpers for consequential CLI writes.
//!
//! Originally lived privately in [`crate::cli::security_cmd`] for the
//! `yolo` / `secure` master switches (#507). Promoted to a shared module for
//! #852 so the same governance — *show the consequence, then require explicit
//! approval* — guards every state-mutating write that can clobber existing
//! state (knowledge overwrites, behavior-changing `config set`), not just the
//! security toggles.
//!
//! Invariant: with no TTY and no `--yes` we **refuse** rather than silently
//! apply. An automated/unattended run must never weaken state without an
//! explicit opt-in flag.

use std::io::Write;

const BOLD: &str = "\x1b[1m";
const YELLOW: &str = "\x1b[33m";
const RST: &str = "\x1b[0m";

/// True if the user passed an explicit approval flag (`-y`, `--yes`,
/// `--force`, `-f`). These bypass the interactive prompt for scripts/CI.
pub(crate) fn wants_yes(args: &[String]) -> bool {
    args.iter()
        .any(|a| matches!(a.as_str(), "-y" | "--yes" | "--force" | "-f"))
}

/// Confirm a consequential change.
///
/// - `assume_yes` short-circuits to `true` (for `--yes` / scripted use).
/// - On a TTY we prompt `[y/N]`: `y`/`yes` is a yes, `n`/`no` or an empty
///   line (Enter) a no. Any other answer is no answer: it is named and the
///   question is asked again, so a mistyped key never counts as a no
///   without a word (rc4-63). After [`MAX_ATTEMPTS`] unclear answers, or
///   when input ends, the answer is no and the person is told so.
/// - With **no** TTY and **no** `--yes` we print a refusal hint and return
///   `false`, so the caller leaves state untouched.
pub(crate) fn confirm(prompt: &str, assume_yes: bool) -> bool {
    if assume_yes {
        return true;
    }
    if !lean_ctx::core::host_env::stdin_is_terminal() {
        eprintln!(
            "{YELLOW}Refusing to apply a consequential change non-interactively.{RST} Re-run with {BOLD}--yes{RST} to confirm."
        );
        return false;
    }
    for _ in 0..MAX_ATTEMPTS {
        print!("{prompt} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        match std::io::stdin().read_line(&mut input) {
            Ok(0) | Err(_) => {
                println!();
                println!("No answer, so that is a no.");
                return false;
            }
            Ok(_) => {}
        }
        match answer(&input) {
            Some(yes) => return yes,
            None => println!(
                "\"{}\" is not an answer. Type y for yes or n for no.",
                shown(&input)
            ),
        }
    }
    println!("No clear answer, so that is a no.");
    false
}

/// How often an unclear answer is asked again before it counts as a no.
const MAX_ATTEMPTS: usize = 3;

/// A typed line as an answer: `Some(true)` for y/yes, `Some(false)` for
/// n/no or Enter alone, `None` for anything else.
fn answer(input: &str) -> Option<bool> {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "" | "n" | "no" => Some(false),
        _ => None,
    }
}

/// The unclear answer as it is shown back: printable, at most 20 chars.
fn shown(input: &str) -> String {
    input
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(20)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_yes_detects_flags() {
        assert!(wants_yes(&["--yes".to_string()]));
        assert!(wants_yes(&["-y".to_string()]));
        assert!(wants_yes(&["--force".to_string()]));
        assert!(wants_yes(&["-f".to_string()]));
        assert!(!wants_yes(&["open".to_string()]));
        assert!(!wants_yes(&[]));
    }

    #[test]
    fn only_yes_and_no_are_answers() {
        assert_eq!(answer("y\n"), Some(true));
        assert_eq!(answer(" YES "), Some(true));
        assert_eq!(answer("n\n"), Some(false));
        assert_eq!(answer("No"), Some(false));
        assert_eq!(answer("\n"), Some(false));
        // rc4-63: a stray key is no answer, so the question is asked again.
        assert_eq!(answer("a\n"), None);
        assert_eq!(answer("ja"), None);
        assert_eq!(answer("yy"), None);
    }

    #[test]
    fn an_unclear_answer_is_shown_back_printable_and_short() {
        assert_eq!(shown("a\n"), "a");
        assert_eq!(shown("\x1b[Aab"), "[Aab");
        assert_eq!(shown(&"x".repeat(50)).len(), 20);
    }

    #[test]
    fn confirm_assume_yes_short_circuits() {
        assert!(confirm("anything", true));
    }
}
