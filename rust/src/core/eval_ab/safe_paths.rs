// SPDX-License-Identifier: Apache-2.0
//! Fixture-path and test-command checks for evaluation suites: a suite may
//! name only plain relative files and run only `sh <relative>.sh`.

use std::path::{Component, Path};

pub(crate) fn is_safe_relative_file(value: &str) -> bool {
    let path = Path::new(value);
    !value.trim().is_empty()
        && !value.as_bytes().contains(&0)
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn is_safe_shell_test(value: &str) -> bool {
    let mut parts = value.split_whitespace();
    let command = parts.next();
    let script = parts.next();
    command == Some("sh")
        && script.is_some_and(|script| {
            Path::new(script)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("sh"))
                && is_safe_relative_file(script)
        })
        && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::{is_safe_relative_file, is_safe_shell_test};

    /// Suites cannot escape the fixture directory or chain shell commands.
    #[test]
    fn suites_name_only_relative_files_and_plain_shell_tests() {
        assert!(is_safe_relative_file("solution.sh"));
        assert!(is_safe_relative_file("src/lib.rs"));
        for bad in [
            "",
            " ",
            "../solution.sh",
            "/etc/passwd",
            "a/../b",
            "./x",
            "a\0b",
        ] {
            assert!(!is_safe_relative_file(bad), "{bad:?}");
        }
        assert!(is_safe_shell_test("sh test.sh"));
        for bad in [
            "sh test.sh; rm -rf /",
            "bash test.sh",
            "sh ../test.sh",
            "sh test.py",
            "sh",
        ] {
            assert!(!is_safe_shell_test(bad), "{bad:?}");
        }
    }
}
