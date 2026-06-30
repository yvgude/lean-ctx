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

/// Decide whether to tee the full output, given the configured [`TeeMode`], the
/// command's `exit_code`, whether the (trimmed) output is blank, and the token
/// counts before/after compression.
///
/// - `Never` never tees.
/// - `Always` tees any non-blank output.
/// - `Failures` tees exactly when the command failed (`exit_code != 0`).
/// - `HighCompression` (the default) is a *superset* of `Failures`: it tees on
///   failure **and** when compression removed >70% of a sizable output. As the
///   default it guarantees the MCP-free recovery path — a real raw file — exists
///   for both the cases an agent actually re-reads: failures and heavily-digested
///   successful runs.
pub(crate) fn should_tee(
    mode: &TeeMode,
    exit_code: i32,
    blank_output: bool,
    original_tokens: usize,
    compressed_tokens: usize,
) -> bool {
    if blank_output {
        return false;
    }
    match mode {
        TeeMode::Never => false,
        TeeMode::Always => true,
        TeeMode::Failures => exit_code != 0,
        TeeMode::HighCompression => {
            exit_code != 0
                || (original_tokens > 100 && savings_pct(original_tokens, compressed_tokens) > 70.0)
        }
    }
}

/// Percentage of tokens removed by compression, clamped to `0.0` when the
/// original was empty. Shared so CLI and MCP report identical savings.
pub(crate) fn savings_pct(original_tokens: usize, compressed_tokens: usize) -> f64 {
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
        assert!(!should_tee(&TeeMode::Never, 1, false, 1000, 10));
        assert!(!should_tee(&TeeMode::Never, 0, false, 1000, 10));
    }

    #[test]
    fn always_mode_tees_non_blank_only() {
        assert!(should_tee(&TeeMode::Always, 0, false, 100, 50));
        assert!(!should_tee(&TeeMode::Always, 0, true, 100, 50));
    }

    #[test]
    fn failures_mode_is_exit_code_based_not_substring() {
        // A non-zero exit tees regardless of how terse / non-"error" the text is
        // (the old substring gate missed `fatal:`, `permission denied`, …).
        assert!(should_tee(&TeeMode::Failures, 1, false, 5, 5));
        assert!(should_tee(&TeeMode::Failures, 127, false, 5, 5));
        // Success never tees, even with large output.
        assert!(!should_tee(&TeeMode::Failures, 0, false, 9999, 10));
        // A blank failure has nothing worth saving.
        assert!(!should_tee(&TeeMode::Failures, 1, true, 0, 0));
    }

    #[test]
    fn high_compression_mode_tees_heavily_digested_output() {
        // >70% savings on sizable output → recoverable.
        assert!(should_tee(&TeeMode::HighCompression, 0, false, 1000, 100));
        // Not enough savings.
        assert!(!should_tee(&TeeMode::HighCompression, 0, false, 1000, 900));
        // Savings high but the output is too small to bother.
        assert!(!should_tee(&TeeMode::HighCompression, 0, false, 80, 1));
    }

    #[test]
    fn high_compression_is_a_superset_of_failures() {
        // As the default tee mode, HighCompression must still tee failures (so the
        // raw-file recovery path exists for them) even when output is tiny and
        // barely compressed — exactly the case `Failures` covered before.
        assert!(should_tee(&TeeMode::HighCompression, 1, false, 5, 5));
        assert!(should_tee(&TeeMode::HighCompression, 127, false, 5, 5));
        // A blank failure still has nothing worth saving.
        assert!(!should_tee(&TeeMode::HighCompression, 1, true, 0, 0));
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
