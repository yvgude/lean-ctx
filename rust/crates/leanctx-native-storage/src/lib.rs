// SPDX-License-Identifier: Apache-2.0
//! Generic native storage primitives, with no licensing or product policy.
//! Unsupported platforms expose no substitute for the Windows authority checks.

#[cfg(windows)]
pub mod windows_file;
#[cfg(windows)]
pub mod windows_private;

#[cfg(any(windows, test))]
fn valid_component(name: &[u16]) -> bool {
    !name.is_empty()
        && !matches!(name.last(), Some(32 | 46))
        && !name
            .iter()
            .any(|unit| *unit < 32 || matches!(*unit, 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124))
}

#[cfg(test)]
mod tests {
    #[test]
    fn rejects_relative_path_escape_and_stream_names() {
        for name in [
            "",
            ".",
            "..",
            "../secret",
            "..\\secret",
            "C:secret",
            "a:b",
            "/root",
            "\\root",
            "a\0b",
            "a.",
            "a ",
        ] {
            assert!(
                !super::valid_component(&name.encode_utf16().collect::<Vec<_>>()),
                "{name:?}"
            );
        }
        for name in ["runtime.json", "κλειδί", "数据", "a..b"] {
            assert!(
                super::valid_component(&name.encode_utf16().collect::<Vec<_>>()),
                "{name:?}"
            );
        }
    }
}
