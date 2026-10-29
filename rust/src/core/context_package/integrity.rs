// SPDX-License-Identifier: Apache-2.0
//! Integrity and signature messages of a `.ctxpkg` document. Every reader
//! — this engine, the hosted registry (`ee/services/cloud/src/ctxpkg_integrity.rs`
//! and the standalone ctxpkg-registry keep byte-identical copies of this file)
//! and conformance tools — recomputes the same values from the writer's bytes
//! instead of trusting what a manifest declares. The conformance vectors under
//! `rust/tests/fixtures/ctxpkg-conformance/` hold every copy to the same bytes.
//!
//! - `content_hash`: SHA-256 of the `content` member's text with insignificant
//!   whitespace removed (spec §8); `byte_size` is that text's length.
//! - `sha256` (the package digest): SHA-256 of `"{name}:{version}:{content_hash}"`.
//! - identity signature message: SHA-256 hex of
//!   `"ctxpkg-sign-v1:{name}:{version}:{sha256}"` (`signature.value`).
//! - manifest signature message: SHA-256 hex of `"ctxpkg-sign-manifest-v1:"`
//!   followed by the canonical JSON (keys sorted, no whitespace) of the
//!   manifest without its `signature` member (`signature.manifest_value`).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// What a reader recomputes from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecomputedIntegrity {
    pub(crate) content_hash: String,
    pub(crate) byte_size: u64,
    pub(crate) sha256: String,
}

/// Recompute the integrity of the document `doc` for the manifest's `name`
/// and `version`; `None` when the document has no top-level `content`.
#[must_use]
pub(crate) fn recompute(doc: &str, name: &str, version: &str) -> Option<RecomputedIntegrity> {
    let content = compact_json_text(member_text(doc, "content")?);
    let content_hash = sha256_hex(content.as_bytes());
    let sha256 = sha256_hex(format!("{name}:{version}:{content_hash}").as_bytes());
    Some(RecomputedIntegrity {
        byte_size: content.len() as u64,
        content_hash,
        sha256,
    })
}

/// The message `signature.value` signs.
#[must_use]
pub(crate) fn identity_signing_message(name: &str, version: &str, sha256: &str) -> String {
    sha256_hex(format!("ctxpkg-sign-v1:{name}:{version}:{sha256}").as_bytes())
}

/// The message `signature.manifest_value` signs, for a manifest as written.
/// The `signature` member is left out whatever it holds.
#[must_use]
pub(crate) fn manifest_signing_message(manifest: &Value) -> String {
    let mut unsigned = manifest.clone();
    if let Some(object) = unsigned.as_object_mut() {
        object.remove("signature");
    }
    let mut hasher = Sha256::new();
    hasher.update(b"ctxpkg-sign-manifest-v1:");
    hasher.update(serde_json::to_vec(&sorted(unsigned)).expect("a JSON value always serializes"));
    hex(&hasher.finalize())
}

/// The exact text of one top-level member's value in a JSON object document,
/// so hashing sees the writer's bytes (spec §8). `None` when the document is
/// not an object or has no such member.
#[must_use]
pub(crate) fn member_text<'a>(doc: &'a str, member: &str) -> Option<&'a str> {
    let bytes = doc.as_bytes();
    let n = bytes.len();
    let mut i = 0;

    let skip_ws = |i: &mut usize| {
        while *i < n && matches!(bytes[*i], b' ' | b'\t' | b'\n' | b'\r') {
            *i += 1;
        }
    };
    let skip_string = |i: &mut usize| {
        *i += 1; // opening quote
        while *i < n {
            match bytes[*i] {
                b'\\' => *i += 2,
                b'"' => {
                    *i += 1;
                    return;
                }
                _ => *i += 1,
            }
        }
    };
    let skip_value = |i: &mut usize| {
        skip_ws(i);
        match bytes.get(*i) {
            Some(b'"') => skip_string(i),
            Some(&open @ (b'{' | b'[')) => {
                let close = if open == b'{' { b'}' } else { b']' };
                let mut depth = 0usize;
                while *i < n {
                    match bytes[*i] {
                        b'"' => {
                            skip_string(i);
                            continue;
                        }
                        c if c == open => depth += 1,
                        c if c == close => {
                            depth -= 1;
                            if depth == 0 {
                                *i += 1;
                                return;
                            }
                        }
                        _ => {}
                    }
                    *i += 1;
                }
            }
            _ => {
                while *i < n
                    && !matches!(bytes[*i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                {
                    *i += 1;
                }
            }
        }
    };

    skip_ws(&mut i);
    if bytes.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    loop {
        skip_ws(&mut i);
        match bytes.get(i) {
            Some(b'"') => {}
            _ => return None,
        }
        let key_start = i;
        skip_string(&mut i);
        let key: String = serde_json::from_str(doc.get(key_start..i)?).ok()?;
        skip_ws(&mut i);
        if bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        skip_ws(&mut i);
        if key == member {
            let start = i;
            skip_value(&mut i);
            return doc.get(start..i);
        }
        skip_value(&mut i);
        skip_ws(&mut i);
        if bytes.get(i) == Some(&b',') {
            i += 1;
        }
    }
}

/// Strip insignificant whitespace outside string literals (spec §8).
#[must_use]
pub(crate) fn compact_json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut in_string = false;
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            match ch {
                '\\' => {
                    if let Some(esc) = chars.next() {
                        out.push(esc);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
        } else {
            match ch {
                '"' => {
                    in_string = true;
                    out.push(ch);
                }
                ' ' | '\t' | '\n' | '\r' => {}
                _ => out.push(ch),
            }
        }
    }
    out
}

fn sorted(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        other => other,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest signature is a signature over these exact bytes; a
    /// serde_json upgrade that changes number or escape layout would make
    /// every published manifest signature fail. Pinned here so that change
    /// fails loudly instead of in the field.
    #[test]
    fn canonical_manifest_bytes_are_pinned() {
        let manifest: Value = serde_json::from_str(
            r#"{"z":"ü\t\"q\"","b":1e21,"a":1.0,"c":0.1,"n":-7,"big":18446744073709551615,
                "nested":{"y":[3,{"k":false}],"x":null},"signature":{"value":"ignored"}}"#,
        )
        .unwrap();
        let mut unsigned = manifest.clone();
        unsigned.as_object_mut().unwrap().remove("signature");
        let bytes = String::from_utf8(serde_json::to_vec(&sorted(unsigned)).unwrap()).unwrap();
        assert_eq!(
            bytes,
            r#"{"a":1.0,"b":1e+21,"big":18446744073709551615,"c":0.1,"n":-7,"nested":{"x":null,"y":[3,{"k":false}]},"z":"ü\t\"q\""}"#
        );
        assert_eq!(manifest_signing_message(&manifest), {
            let mut hasher = Sha256::new();
            hasher.update(b"ctxpkg-sign-manifest-v1:");
            hasher.update(bytes.as_bytes());
            hex(&hasher.finalize())
        });
    }
}
