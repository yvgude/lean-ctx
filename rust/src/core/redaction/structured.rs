// SPDX-License-Identifier: Apache-2.0
//! Structural masking of JSON that a released document carries: the whole
//! value of a member with a credential's name, at any depth, whatever its
//! shape.
//!
//! The text rules of `super::redaction_rules` read a secret by the name in
//! front of it (`"token":"…"`), so they only see a STRING right after the
//! name. A secret one level further down (`{"token":["S"]}`,
//! `{"auth":{"value":"S"}}`) or kept as a number or a boolean passed every
//! text rule unmasked. Here a member whose name reads as a credential
//! ([`is_secret_key`]) has its whole value replaced by [`SECRET_MEMBER_MARKER`]:
//! arrays, objects, numbers and booleans alike, the names inside included.
//! Only `null`, `""` and a value that is already a mask stay as they are
//! (allowlist; every other shape is masked).
//!
//! A string value at any depth that is itself a JSON object or array (a fact
//! whose value is a serialized document) is masked the same way, and the
//! masked JSON stands in the string. The value detectors (provider tokens,
//! key/value rules) still run on every string leaf where the caller releases
//! the document; this pass only adds what they cannot see.

use serde_json::Value;

/// What stands in place of the whole value of a credential member. It starts
/// with `[REDACTED` like every marker of the text rules, so callers that
/// locate masked spans by that prefix see it as a mask.
pub const SECRET_MEMBER_MARKER: &str = "[REDACTED:Secret member]";

/// Nesting deeper than this is not inspected: the document is refused.
const MAX_DEPTH: usize = 64;

/// Embedded JSON candidates in free text that fail to parse before the scan
/// gives up and refuses the text (bounds the work on hostile input).
const MAX_FAILED_CANDIDATES: usize = 4096;

/// Why a text could not be inspected; the caller withholds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uninspectable {
    /// Nested deeper than the inspection depth.
    TooDeep,
    /// Too many JSON-like fragments that do not parse.
    TooManyFragments,
}

/// A masked text and the masked string values that are worth looking for
/// elsewhere in the delivered text (benign literals such as `none` are masked
/// but not listed: finding them elsewhere proves nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Masked {
    pub text: String,
    pub originals: Vec<String>,
}

impl Masked {
    /// Whether a masked value of four or more characters still appears in the
    /// masked text, as written or as JSON escapes it (a copy under another
    /// name, or inside another value). Such a text must be withheld.
    pub fn still_carries_an_original(&self) -> bool {
        self.originals
            .iter()
            .filter(|original| original.chars().count() >= 4)
            .any(|original| {
                let escaped = serde_json::to_string(original).unwrap_or_default();
                let escaped = escaped
                    .strip_prefix('"')
                    .and_then(|inner| inner.strip_suffix('"'))
                    .unwrap_or(&escaped);
                self.text.contains(original.as_str()) || self.text.contains(escaped)
            })
    }
}

/// Credential words that make a member name secret when they END it.
const SECRET_LAST_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "passcode",
    "secret",
    "secrets",
    "token",
    "credential",
    "credentials",
    "auth",
    "authorization",
    "bearer",
    "cookie",
    "apikey",
    "privatekey",
    "secretkey",
    "accesskey",
];

/// Qualifiers that make a final `key` a credential (`api_key`,
/// `private_key`). A bare `key` (a knowledge fact's slug) and a
/// `public_key` stay readable.
const SECRET_KEY_QUALIFIERS: &[&str] = &[
    "api",
    "access",
    "secret",
    "private",
    "signing",
    "encryption",
    "master",
    "client",
    "session",
];

/// The words of a member name: split at every non-alphanumeric character and
/// at camelCase boundaries (`accessToken`, `APIKey`, `x-api-key`), lowercased.
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if ch.is_uppercase() && !current.is_empty() {
            let previous = chars[index - 1];
            let next_is_lower = chars.get(index + 1).is_some_and(|c| c.is_lowercase());
            if previous.is_lowercase()
                || previous.is_ascii_digit()
                || (previous.is_uppercase() && next_is_lower)
            {
                words.push(std::mem::take(&mut current));
            }
        }
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Whether a JSON member name reads as a credential: its last word is one
/// (`token`, `password`, `auth`, `client_secret`, `accessToken`), or it ends
/// in `key` with a credential qualifier (`api_key`, `private_key`). Counts
/// and metadata (`tokens`, `max_tokens`, `token_type`, `secret_count`), a bare
/// `key` and `public_key` are not.
pub fn is_secret_key(name: &str) -> bool {
    let words = name_words(name);
    let Some(last) = words.last() else {
        return false;
    };
    if SECRET_LAST_WORDS.contains(&last.as_str()) {
        return true;
    }
    last == "key"
        && words.len() >= 2
        && SECRET_KEY_QUALIFIERS.contains(&words[words.len() - 2].as_str())
}

/// A value under a credential name that is left as it is: nothing in it can
/// be a secret.
fn is_empty_or_mask(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => {
            text.is_empty()
                || (text.starts_with("[REDACTED") && text.ends_with(']') && !text.contains('\n'))
        }
        _ => false,
    }
}

/// Collect the string leaves of a masked subtree that are worth looking for
/// elsewhere.
fn collect_originals(value: &Value, originals: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if !super::is_benign_quoted_value(text) {
                originals.push(text.clone());
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_originals(item, originals)),
        Value::Object(map) => map
            .values()
            .for_each(|item| collect_originals(item, originals)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Mask, in place, the whole value of every member of `value` whose name is a
/// credential ([`is_secret_key`]), at any depth, and every JSON document kept
/// as a string value. Returns whether anything changed; the masked string
/// values worth looking for elsewhere are appended to `originals`.
pub fn mask_secret_members(
    value: &mut Value,
    originals: &mut Vec<String>,
) -> Result<bool, Uninspectable> {
    mask_value(value, 0, originals)
}

fn mask_value(
    value: &mut Value,
    depth: usize,
    originals: &mut Vec<String>,
) -> Result<bool, Uninspectable> {
    if depth > MAX_DEPTH {
        return Err(Uninspectable::TooDeep);
    }
    match value {
        Value::Object(map) => {
            let mut changed = false;
            for (name, item) in map.iter_mut() {
                if is_secret_key(name) {
                    if !is_empty_or_mask(item) {
                        collect_originals(item, originals);
                        *item = Value::String(SECRET_MEMBER_MARKER.to_owned());
                        changed = true;
                    }
                } else {
                    changed |= mask_value(item, depth + 1, originals)?;
                }
            }
            Ok(changed)
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= mask_value(item, depth + 1, originals)?;
            }
            Ok(changed)
        }
        Value::String(text) => {
            if let Some(mut inner) = embedded_document(text) {
                if !mask_value(&mut inner, depth + 1, originals)? {
                    return Ok(false);
                }
                *text = inner.to_string();
                return Ok(true);
            }
            // JSON after other text in the value (`creds: {"token":["S"]}`)
            // is masked where it stands, as it is in free text.
            match mask_embedded_json(text, depth + 1, originals)? {
                Some(masked) => {
                    *text = masked;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(false),
    }
}

/// The JSON object or array a string value holds as its whole text.
fn embedded_document(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
}

/// [`mask_secret_members`] for a text: a text that is one JSON document is
/// masked as a value and written back in its own layout (pretty when it
/// spans lines); any other text has every embedded JSON object or array
/// masked where it stands. `Ok(None)`: nothing to mask, the text keeps its
/// bytes.
pub fn mask_structured_text(text: &str) -> Result<Option<Masked>, Uninspectable> {
    let mut originals = Vec::new();
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        if !mask_secret_members(&mut value, &mut originals)? {
            return Ok(None);
        }
        let serialized = if text.contains('\n') {
            serde_json::to_string_pretty(&value)
        } else {
            serde_json::to_string(&value)
        };
        let mut masked = serialized.map_err(|_| Uninspectable::TooDeep)?;
        if text.ends_with('\n') && !masked.ends_with('\n') {
            masked.push('\n');
        }
        return Ok(Some(Masked {
            text: masked,
            originals,
        }));
    }
    mask_embedded_json(text, 0, &mut originals)
        .map(|masked| masked.map(|text| Masked { text, originals }))
}

/// Whether the byte at `at` may start an embedded JSON object or array worth
/// parsing: `{` followed by a member name, or `[` followed by a container or
/// a string.
fn starts_candidate(bytes: &[u8], at: usize) -> bool {
    let next = bytes[at + 1..]
        .iter()
        .copied()
        .find(|b| !b.is_ascii_whitespace());
    matches!(
        (bytes[at], next),
        (b'{', Some(b'"')) | (b'[', Some(b'{' | b'[' | b'"'))
    )
}

/// Mask every JSON object or array embedded in `text` where it stands, at
/// nesting `depth` (a text inside a string value counts the levels above it).
fn mask_embedded_json(
    text: &str,
    depth: usize,
    originals: &mut Vec<String>,
) -> Result<Option<String>, Uninspectable> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut at = 0;
    let mut failed = 0usize;
    let mut changed = false;
    while at < bytes.len() {
        if !matches!(bytes[at], b'{' | b'[') || !starts_candidate(bytes, at) {
            at += 1;
            continue;
        }
        let mut stream = serde_json::Deserializer::from_str(&text[at..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(mut value)) => {
                let end = at + stream.byte_offset();
                if mask_value(&mut value, depth, originals)? {
                    out.push_str(&text[copied..at]);
                    out.push_str(&value.to_string());
                    copied = end;
                    changed = true;
                }
                at = end;
            }
            Some(Err(error))
                if error.classify() == serde_json::error::Category::Syntax
                    && error.to_string().contains("recursion limit") =>
            {
                return Err(Uninspectable::TooDeep);
            }
            _ => {
                failed += 1;
                if failed > MAX_FAILED_CANDIDATES {
                    return Err(Uninspectable::TooManyFragments);
                }
                at += 1;
            }
        }
    }
    if !changed {
        return Ok(None);
    }
    out.push_str(&text[copied..]);
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A credential-shaped value built at run time, so no fixture in the
    /// repository looks like a real token to a secret scanner.
    fn secret() -> String {
        ["q7", "Zr", "Lm", "x2", "Vb", "8n", "Tk", "w4"].concat()
    }

    fn masked(value: Value) -> (Value, Vec<String>) {
        let mut value = value;
        let mut originals = Vec::new();
        mask_secret_members(&mut value, &mut originals).expect("inspectable");
        (value, originals)
    }

    #[test]
    fn secret_names_are_recognized_and_metadata_names_are_not() {
        for name in [
            "token",
            "password",
            "auth",
            "Authorization",
            "client_secret",
            "accessToken",
            "refresh-token",
            "APIKey",
            "api_key",
            "x-api-key",
            "private_key",
            "AWS_SECRET_ACCESS_KEY",
            "db_password",
            "secrets",
            "credentials",
        ] {
            assert!(is_secret_key(name), "{name} must be secret");
        }
        for name in [
            "key",
            "public_key",
            "signer_public_key",
            "tokens",
            "max_tokens",
            "input_tokens",
            "token_type",
            "token_count",
            "secret_count",
            "author",
            "oauth_provider",
            "category",
            "value",
            "keys",
        ] {
            assert!(!is_secret_key(name), "{name} must stay readable");
        }
    }

    #[test]
    fn every_shape_under_a_secret_name_is_masked() {
        let s = secret();
        let (value, originals) = masked(json!({
            "token": [s],
            "auth": {"value": s},
            "passcode": 1234,
            "api_key": true,
            "password": null,
            "secret": "",
        }));
        assert_eq!(value["token"], SECRET_MEMBER_MARKER);
        assert_eq!(value["auth"], SECRET_MEMBER_MARKER);
        assert_eq!(value["passcode"], SECRET_MEMBER_MARKER);
        assert_eq!(value["api_key"], SECRET_MEMBER_MARKER);
        assert_eq!(value["password"], Value::Null);
        assert_eq!(value["secret"], "");
        assert!(originals.iter().all(|o| o == &s) && originals.len() == 2);
        assert!(!value.to_string().contains(&s));
    }

    #[test]
    fn deep_nesting_arrays_of_objects_and_embedded_documents_are_masked() {
        let s = secret();
        let embedded = json!({"nested": {"credentials": {"user": "u", "value": s}}}).to_string();
        let (value, _) = masked(json!({
            "a": {"b": {"c": {"d": [{"e": {"client_secret": {"x": [s]}}}]}}},
            "rows": [{"name": "one", "token": s}, {"name": "two", "token": [s, s]}],
            "value": embedded,
        }));
        let text = value.to_string();
        assert!(!text.contains(&s), "{text}");
        assert_eq!(value["rows"][0]["name"], "one");
        assert_eq!(value["rows"][1]["name"], "two");
        assert!(value["value"].as_str().unwrap().contains("Secret member"));
    }

    #[test]
    fn readable_documents_keep_their_bytes() {
        let clean = r#"{"facts":[{"category":"auth","key":"token-type","value":"bearer, rotated daily","tokens":42,"public_key":"abc"}]}"#;
        assert_eq!(mask_structured_text(clean), Ok(None));
        let pretty =
            serde_json::to_string_pretty(&serde_json::from_str::<Value>(clean).unwrap()).unwrap();
        assert_eq!(mask_structured_text(&pretty), Ok(None));
        assert_eq!(mask_structured_text("plain text {not json}"), Ok(None));
    }

    #[test]
    fn json_embedded_in_free_text_is_masked_where_it_stands() {
        let s = secret();
        let text = format!(
            "[auth] creds: {} (kept) and {{\"name\":\"ok\"}}",
            json!({"auth": {"value": s}})
        );
        let out = mask_structured_text(&text).unwrap().expect("masked");
        assert!(!out.text.contains(&s));
        assert!(
            out.text
                .starts_with("[auth] creds: {\"auth\":\"[REDACTED:Secret member]\"}")
        );
        assert!(out.text.ends_with("(kept) and {\"name\":\"ok\"}"));
        assert_eq!(out.originals, vec![s]);
    }

    /// !427 review B3: JSON after other text in a string value
    /// (`"creds: {\"token\":[\"S\"]}"`) was released: only a value that is
    /// a JSON document as a whole was inspected.
    #[test]
    fn json_after_other_text_in_a_string_value_is_masked() {
        let s = secret();
        let inner = format!("creds: {}", json!({"token": [s]}));
        let (value, originals) = masked(json!([{"key": "c", "value": inner}]));
        let text = value[0]["value"].as_str().expect("string");
        assert!(!text.contains(&s), "{text}");
        assert_eq!(text, "creds: {\"token\":\"[REDACTED:Secret member]\"}");
        assert_eq!(value[0]["key"], "c");
        assert_eq!(originals, vec![s.clone()]);
        let document = json!([{"key": "c", "value": inner}]).to_string();
        let out = mask_structured_text(&document).unwrap().expect("masked");
        assert!(!out.text.contains(&s), "{}", out.text);
        // The same text on its own (free text) is masked as well.
        let out = mask_structured_text(&inner).unwrap().expect("masked");
        assert!(!out.text.contains(&s), "{}", out.text);
        // A readable value with braces keeps its bytes.
        let clean = json!([{"value": "see {\"name\":\"ok\"} and [\"a\"]"}]).to_string();
        assert_eq!(mask_structured_text(&clean), Ok(None));
    }

    #[test]
    fn too_deep_documents_are_refused() {
        let deep = format!("{}{}", "[".repeat(80), "]".repeat(80));
        let mut value: Value = serde_json::from_str(&deep).unwrap();
        assert_eq!(
            mask_secret_members(&mut value, &mut Vec::new()),
            Err(Uninspectable::TooDeep)
        );
    }
}
