//! Single source of truth for the shell "tee" decision — whether the full,
//! pre-compression output is saved to a recovery file so the agent can retrieve
//! it instead of re-running the command.
//!
//! Both the CLI buffered path (`shell::exec`) and the MCP `ctx_shell` handler
//! call [`should_tee`], so `TeeMode::Failures` means the exact same thing on
//! both: a non-zero exit code — never a brittle substring match on the word
//! "error" (which misses `fatal:`, `permission denied`, localized messages, and
//! terse failures). See #809 / #811.

use crate::core::config::TeeMode;

/// Decide whether to persist a recovery copy of shell output.
pub fn should_tee(
    mode: &TeeMode,
    exit_code: i32,
    blank_output: bool,
    content_elided: bool,
    original_tokens: usize,
    compressed_tokens: usize,
) -> bool {
    if blank_output {
        return false;
    }
    match mode {
        TeeMode::Never => false,
        // Explicit retention mode remains independent of compression.
        TeeMode::Always => true,
        TeeMode::Failures => exit_code != 0 && content_elided,
        TeeMode::HighCompression => {
            content_elided
                && (exit_code != 0
                    || (original_tokens > 100
                        && savings_pct(original_tokens, compressed_tokens) > 70.0))
        }
    }
}

/// True only when the inline representation does not contain the complete
/// non-blank output. Wrappers such as `<error>…</error>` do not count as
/// elision.
pub fn output_was_elided(full_output: &str, inline_output: &str) -> bool {
    let full_output = full_output.trim_end_matches(['\r', '\n']);
    !full_output.is_empty() && !inline_output.contains(full_output)
}

/// Percentage of tokens removed by compression, clamped to `0.0` when the
/// original was empty. Shared so CLI and MCP report identical savings.
pub fn savings_pct(original_tokens: usize, compressed_tokens: usize) -> f64 {
    if original_tokens == 0 {
        return 0.0;
    }
    (original_tokens.saturating_sub(compressed_tokens) as f64 / original_tokens as f64) * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_mode_never_tees() {
        assert!(!should_tee(&TeeMode::Never, 1, false, true, 1000, 10));
        assert!(!should_tee(&TeeMode::Never, 0, false, true, 1000, 10));
    }

    #[test]
    fn always_mode_tees_non_blank_only() {
        assert!(should_tee(&TeeMode::Always, 0, false, false, 100, 50));
        assert!(!should_tee(&TeeMode::Always, 0, true, true, 100, 50));
    }

    #[test]
    fn failures_tee_only_when_output_was_elided() {
        assert!(should_tee(&TeeMode::Failures, 1, false, true, 5, 2));
        assert!(!should_tee(&TeeMode::Failures, 1, false, false, 5, 5));
        assert!(!should_tee(&TeeMode::Failures, 0, false, true, 9999, 10));
        assert!(!should_tee(&TeeMode::Failures, 1, true, true, 0, 0));
    }

    #[test]
    fn high_compression_requires_actual_elision() {
        assert!(should_tee(
            &TeeMode::HighCompression,
            0,
            false,
            true,
            1000,
            100
        ));
        assert!(!should_tee(
            &TeeMode::HighCompression,
            0,
            false,
            false,
            1000,
            100
        ));
        assert!(!should_tee(
            &TeeMode::HighCompression,
            0,
            false,
            true,
            1000,
            900
        ));
        assert!(!should_tee(
            &TeeMode::HighCompression,
            1,
            false,
            false,
            5,
            5
        ));
    }

    #[test]
    fn detects_full_output_inside_error_wrapper() {
        assert!(!output_was_elided("hi\n", "<error>hi\n[exit:1]</error>"));
        assert!(output_was_elided(
            "first\nmissing\nlast\n",
            "first\n…\nlast"
        ));
        assert!(!output_was_elided("", ""));
    }

    #[test]
    fn default_tee_mode_is_high_compression() {
        assert_eq!(TeeMode::default(), TeeMode::HighCompression);
    }

    #[test]
    fn savings_pct_handles_zero_original() {
        assert_eq!(savings_pct(0, 0), 0.0);
        assert!((savings_pct(1000, 100) - 90.0).abs() < f64::EPSILON);
    }
}
