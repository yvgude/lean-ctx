// SPDX-License-Identifier: Apache-2.0

//! Content-based "keep verbatim" guard (governor design v1 §2 G).
//!
//! `output_policy::classify` decides by *command* and classes `cargo test` /
//! `npm test` as compressible, so it cannot promise that errors, diffs and
//! test results stay verbatim. This guard looks at the *content*: any error
//! word (the shared `log_dedup::error_re`), a traceback, a diff hunk or a
//! test-runner line keeps the whole result untouched.

fn extra_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?m)",
            // Failure and assertion words (error words come from error_re).
            r"(?i:\b(fail(ed|ure|ures|ing)?|assert(ion|ions|ed)?|panick(ed|ing)|warning|denied|traceback|segfault|abort(ed)?)\b)",
            // Diff hunks and headers.
            r"|^diff --git |^@@ |^\+\+\+ |^--- |^index [0-9a-f]+\.\.[0-9a-f]+",
            // Test-runner summaries and per-test lines.
            r"|(?i:\btest result:|\brunning \d+ tests?\b|\b\d+ (passed|skipped|tests?)\b)",
            r"|^(PASS|FAIL|ok|not ok)\b",
        ))
        .expect("BUG: invalid governor diagnostics regex")
    })
}

/// True when `text` must stay verbatim.
pub(super) fn has_diagnostics(text: &str) -> bool {
    crate::core::patterns::log_dedup::error_re().is_match(text) || extra_re().is_match(text)
}

#[cfg(test)]
mod tests {
    use super::has_diagnostics;

    #[test]
    fn diagnostics_table() {
        let cases: &[(&str, bool)] = &[
            ("src/main.rs\nsrc/lib.rs\n", false),
            ("total 8\ndrwxr-xr-x  2 u g 64 .\n", false),
            ("pending_errors.rs\n", false),
            ("error[E0308]: mismatched types\n", true),
            ("thread 'main' panicked at src/x.rs:1:1\n", true),
            ("Traceback (most recent call last):\n", true),
            ("diff --git a/x b/x\n", true),
            ("@@ -1,3 +1,4 @@\n", true),
            ("test result: ok. 12 passed; 0 failed\n", true),
            ("running 3 tests\n", true),
            ("FAIL src/a.test.ts\n", true),
            ("assertion `left == right` failed\n", true),
            ("warning: unused variable\n", true),
        ];
        for (text, want) in cases {
            assert_eq!(has_diagnostics(text), *want, "{text:?}");
        }
    }
}
