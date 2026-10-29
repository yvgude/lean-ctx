// SPDX-License-Identifier: Apache-2.0

//! Code anchors (C0 1.9): where in the code a fact, or a piece of task
//! evidence, applies.
//!
//! An anchor names a repo-relative `path`, optionally a `symbol` in it, and
//! the `digest` of what it covered when it was written. The engine resolves
//! anchors against its local code graph; Hive only stores them.
//!
//! The digest (section 3) is [`anchor_digest`] of the covered bytes: the
//! exact bytes of the symbol's syntax node (without the comments before it),
//! or of the whole file without a symbol, normalized by
//! [`normalize_anchor_content`]. `vectors.json` → `anchor_digest` holds the
//! reference cases every implementation reproduces.
//!
//! In sealed vaults anchors travel only inside sealed content: a path or a
//! symbol name is information too.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ValidationError;

/// The covered content as it is hashed: a leading UTF-8 byte-order mark
/// dropped, every `\r\n` and lone `\r` turned into `\n`, and trailing spaces
/// and tabs removed from every line. Nothing else changes: indentation,
/// blank lines and the final newline (or its absence) stay.
pub fn normalize_anchor_content(content: &[u8]) -> Vec<u8> {
    let content = content.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(content);
    let mut lf = Vec::with_capacity(content.len());
    let mut bytes = content.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte == b'\r' {
            if bytes.peek() == Some(&b'\n') {
                bytes.next();
            }
            lf.push(b'\n');
        } else {
            lf.push(byte);
        }
    }
    let mut out = Vec::with_capacity(lf.len());
    for (i, line) in lf.split(|b| *b == b'\n').enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        let end = line
            .iter()
            .rposition(|b| *b != b' ' && *b != b'\t')
            .map_or(0, |last| last + 1);
        out.extend_from_slice(&line[..end]);
    }
    out
}

/// `sha256:` + lowercase hex of the normalized content.
pub fn anchor_digest(content: &[u8]) -> String {
    let digest = Sha256::digest(normalize_anchor_content(content));
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub const MAX_ANCHOR_PATH_BYTES: usize = 512;
pub const MAX_ANCHOR_SYMBOL_CHARS: usize = 256;
pub const MAX_ANCHORS: usize = 16;

/// One code anchor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeAnchorV1 {
    /// Repo-relative, `/`-separated, NFC; no `.` or `..` segment, no leading
    /// or trailing `/`.
    pub path: String,
    /// Language-neutral qualified name from the code graph
    /// (`hive::policy::verify_bundle`, `AuthService.verify`); the whole file
    /// when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// `sha256:` + 64 lowercase hex over the normalized symbol span (or the
    /// whole file without a symbol).
    pub digest: String,
    /// Tree-sitter language id, a hint only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
}

/// Characters that could make two paths look alike: every one a text field
/// refuses ([`super::invisible`]) and, in a path, the joiners as well.
fn invisible(c: char) -> bool {
    super::invisible(c) || matches!(c, '\u{200C}' | '\u{200D}')
}

/// A repo-relative, `/`-separated path without `.` or `..` segments (the
/// rule for anchors and, 1.9 section 8, the code graph). 1.9 R3: no `:`
/// (drive letters, alternate data streams) and no invisible format
/// characters.
pub fn validate_anchor_path(path: &str) -> Result<(), ValidationError> {
    if path.is_empty()
        || path.len() > MAX_ANCHOR_PATH_BYTES
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path.chars().any(|c| c.is_control() || invisible(c))
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ValidationError::new(
            "anchor path must be a normalized repo-relative path",
        ));
    }
    Ok(())
}

impl CodeAnchorV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_anchor_path(&self.path)?;
        if let Some(symbol) = &self.symbol
            && (symbol.is_empty()
                || symbol.chars().count() > MAX_ANCHOR_SYMBOL_CHARS
                || symbol.trim() != symbol
                || symbol.chars().any(char::is_control))
        {
            return Err(ValidationError::new(
                "anchor symbol must be 1-256 characters without surrounding spaces",
            ));
        }
        let hex = self
            .digest
            .strip_prefix("sha256:")
            .ok_or_else(|| ValidationError::new("anchor digest must start with sha256:"))?;
        super::validate_hex64(hex, "anchor digest")?;
        if let Some(lang) = &self.lang
            && (lang.is_empty()
                || lang.len() > 32
                || !lang
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
        {
            return Err(ValidationError::new(
                "anchor lang must be 1-32 characters of [a-z0-9_]",
            ));
        }
        Ok(())
    }
}

/// A list of anchors: at most [`MAX_ANCHORS`], sorted by `(path, symbol)`
/// (no symbol first), without repeats.
pub fn validate_anchors(anchors: &[CodeAnchorV1]) -> Result<(), ValidationError> {
    if anchors.len() > MAX_ANCHORS {
        return Err(ValidationError::new(format!(
            "at most {MAX_ANCHORS} anchors"
        )));
    }
    for (i, anchor) in anchors.iter().enumerate() {
        anchor.validate()?;
        if i > 0 {
            let prev = &anchors[i - 1];
            if (prev.path.as_str(), prev.symbol.as_deref())
                >= (anchor.path.as_str(), anchor.symbol.as_deref())
            {
                return Err(ValidationError::new(
                    "anchors must be sorted by path and symbol without repeats",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(path: &str, symbol: Option<&str>) -> CodeAnchorV1 {
        CodeAnchorV1 {
            path: path.into(),
            symbol: symbol.map(Into::into),
            digest: format!("sha256:{}", "a".repeat(64)),
            lang: None,
        }
    }

    #[test]
    fn paths_are_normalized_and_repo_relative() {
        assert!(
            anchor("src/retry.rs", Some("retry::budget"))
                .validate()
                .is_ok()
        );
        for bad in [
            "/etc/x",
            "src/",
            "a//b",
            "./a",
            "a/../b",
            "a\\b",
            "",
            "C:/x",
            "a.txt:stream",
            "a\u{202E}b",
        ] {
            assert!(anchor(bad, None).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn anchor_lists_are_sorted_without_repeats() {
        let a = anchor("a.rs", None);
        let a_sym = anchor("a.rs", Some("f"));
        let b = anchor("b.rs", None);
        assert!(validate_anchors(&[a.clone(), a_sym.clone(), b.clone()]).is_ok());
        assert!(validate_anchors(&[a_sym.clone(), a.clone()]).is_err());
        assert!(validate_anchors(&[b.clone(), b]).is_err());
    }
}
