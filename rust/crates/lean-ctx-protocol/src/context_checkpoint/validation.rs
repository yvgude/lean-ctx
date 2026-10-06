// SPDX-License-Identifier: Apache-2.0

use super::*;

pub(crate) fn require_capacity(len: usize, cap: usize, field: &str) -> Result<(), ValidationError> {
    if len > cap {
        return Err(ValidationError::new(format!(
            "{field} exceeds the {cap} item limit"
        )));
    }
    Ok(())
}

pub(crate) fn require_sorted_unique<T>(
    items: &[T],
    field: &str,
    key: impl Fn(&T) -> &str,
) -> Result<(), ValidationError> {
    let mut previous: Option<&str> = None;
    for item in items {
        let current = key(item);
        if previous.is_some_and(|previous| previous >= current) {
            return Err(ValidationError::new(format!(
                "{field} must be sorted by identity and unique"
            )));
        }
        previous = Some(current);
    }
    Ok(())
}

pub(crate) fn require_unique<T>(
    items: &[T],
    field: &str,
    key: impl Fn(&T) -> &str,
) -> Result<(), ValidationError> {
    let mut seen = std::collections::BTreeSet::new();
    for item in items {
        if !seen.insert(key(item)) {
            return Err(ValidationError::new(format!(
                "{field} must not contain duplicates"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_reference_list(
    refs: &[ProtocolReference],
    field: &str,
) -> Result<(), ValidationError> {
    require_capacity(refs.len(), MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS, field)?;
    for reference in refs {
        validate_checkpoint_reference(reference.as_str(), field)?;
    }
    require_sorted_unique(refs, field, ProtocolReference::as_str)
}

/// Validate a canonical lowercase hyphenated UUID and reject the nil UUID.
pub(super) fn validate_canonical_uuid(value: &str, field: &str) -> Result<(), ValidationError> {
    let bytes = value.as_bytes();
    let hyphens = [8usize, 13, 18, 23];
    if bytes.len() != 36
        || hyphens.iter().any(|index| bytes[*index] != b'-')
        || bytes.iter().enumerate().any(|(index, byte)| {
            !hyphens.contains(&index) && !(byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        })
    {
        return Err(ValidationError::new(format!(
            "{field} must be a canonical lowercase hyphenated UUID"
        )));
    }
    if bytes.iter().all(|byte| *byte == b'0' || *byte == b'-') {
        return Err(ValidationError::new(format!(
            "{field} must not be the nil UUID"
        )));
    }
    Ok(())
}

/// Validate an existing protocol identity in canonical lowercase form.
pub(crate) fn validate_checkpoint_identifier(
    value: &str,
    field: &str,
) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > MAX_CONTEXT_CHECKPOINT_ID_BYTES
        || !value.is_ascii()
        || value != value.to_ascii_lowercase()
        || !value.as_bytes()[0].is_ascii_lowercase() && !value.as_bytes()[0].is_ascii_digit()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._:-".contains(&byte)
        })
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.contains('%')
    {
        return Err(ValidationError::new(format!(
            "{field} must be a canonical lowercase protocol identity of at most {MAX_CONTEXT_CHECKPOINT_ID_BYTES} bytes"
        )));
    }
    reject_machine_local(value, field)
}

/// Validate a bounded lowercase slug that cannot spell a path or key material.
pub(super) fn validate_checkpoint_slug(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.is_empty()
        || value.len() > MAX_CONTEXT_CHECKPOINT_SLUG_BYTES
        || !value.as_bytes()[0].is_ascii_lowercase() && !value.as_bytes()[0].is_ascii_digit()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
    {
        return Err(ValidationError::new(format!(
            "{field} must be a bounded lowercase slug of at most {MAX_CONTEXT_CHECKPOINT_SLUG_BYTES} bytes"
        )));
    }
    reject_machine_local(value, field)
}

/// Validate a bounded single-line free-text value.
pub(super) fn validate_checkpoint_text(value: &str, field: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::new(format!("{field} must not be empty")));
    }
    if value.len() > MAX_CONTEXT_CHECKPOINT_TEXT_BYTES {
        return Err(ValidationError::new(format!(
            "{field} exceeds the {MAX_CONTEXT_CHECKPOINT_TEXT_BYTES} byte limit"
        )));
    }
    if !value.is_ascii()
        || value.contains(['\u{2028}', '\u{2029}'])
        || value
            .chars()
            .any(|character| character.is_control() || is_unicode_format_character(character))
    {
        return Err(ValidationError::new(format!(
            "{field} must use canonical ASCII without control, line-separator, or Unicode format characters"
        )));
    }
    if value != value.trim() {
        return Err(ValidationError::new(format!(
            "{field} must not have leading or trailing whitespace"
        )));
    }
    reject_machine_local(value, field)
}

/// Validate a bounded, scheme-prefixed portable protocol reference.
pub(crate) fn validate_checkpoint_reference(
    value: &str,
    field: &str,
) -> Result<(), ValidationError> {
    let Some((scheme, rest)) = value.split_once(':') else {
        return Err(ValidationError::new(format!(
            "{field} must use a <scheme>:<value> reference form"
        )));
    };
    if value.len() > MAX_CONTEXT_CHECKPOINT_REFERENCE_BYTES
        || scheme.is_empty()
        || scheme.len() > 32
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || rest.is_empty()
        || !value.is_ascii()
        || value != value.to_ascii_lowercase()
        || value.chars().any(|character| {
            character.is_control() || character.is_whitespace() || character == '\u{feff}'
        })
        || value.contains('\\')
        || !matches!(
            scheme,
            "artifact"
                | "evidence"
                | "gotcha"
                | "knowledge"
                | "kms"
                | "learning"
                | "package"
                | "plan"
                | "policy"
                | "project"
                | "receipt"
                | "revision"
                | "session"
                | "snapshot"
                | "source"
                | "task"
        )
        || (rest.starts_with("//") && !is_safe_hierarchical_reference(value, scheme, rest))
        || (!rest.starts_with("//") && rest.contains('/'))
        || rest.starts_with('~')
        || rest.contains('@')
        || rest.contains("..")
        || rest.contains('%')
    {
        return Err(ValidationError::new(format!(
            "{field} must be a bounded scheme-prefixed portable reference"
        )));
    }
    reject_machine_local(value, field)
}

/// Reject machine-local, process-local, and credential-shaped material.
pub(super) fn reject_machine_local(value: &str, field: &str) -> Result<(), ValidationError> {
    if !value.is_ascii() || value.chars().any(is_unicode_format_character) {
        return Err(ValidationError::new(format!(
            "{field} must not contain Unicode format characters"
        )));
    }
    reject_machine_local_content(value, field)
}

/// Shared content scan after the versioned text character policy.
pub(crate) fn reject_machine_local_content(
    value: &str,
    field: &str,
) -> Result<(), ValidationError> {
    if value
        .split_whitespace()
        .map(trim_token_punctuation)
        .any(is_credential_shaped_token)
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain credential-shaped tokens"
        )));
    }
    let lowered = value.to_ascii_lowercase();
    if lowered.starts_with('/')
        || lowered.starts_with('~')
        || lowered.starts_with("./")
        || lowered.starts_with("../")
        || lowered.contains('\\')
        || is_windows_drive_path(&lowered)
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain a filesystem path"
        )));
    }
    if lowered.split_whitespace().any(is_absolute_path_token)
        || lowered.contains("file:///")
        || contains_embedded_absolute_path(&lowered)
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain an absolute filesystem path"
        )));
    }
    if let Some(marker) = REJECTED_ASSIGNMENTS
        .iter()
        .find(|marker| lowered.contains(**marker))
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain machine-local or credential material ({marker})"
        )));
    }
    if contains_spaced_sensitive_assignment(&lowered) {
        return Err(ValidationError::new(format!(
            "{field} must not contain credential assignments"
        )));
    }
    if lowered.split_whitespace().any(|token| {
        REJECTED_TOKEN_PREFIXES
            .iter()
            .any(|prefix| token.starts_with(prefix))
    }) || lowered
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        })
        .filter(|fragment| !fragment.is_empty())
        .any(|fragment| {
            REJECTED_TOKEN_PREFIXES
                .iter()
                .any(|prefix| fragment.starts_with(prefix))
                || is_credential_shaped_token(fragment)
        })
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain credential-shaped tokens"
        )));
    }
    if lowered
        .split_whitespace()
        .map(trim_token_punctuation)
        .any(|token| is_credential_shaped_token(token) || is_sensitive_assignment(token))
    {
        return Err(ValidationError::new(format!(
            "{field} must not contain credential-shaped tokens"
        )));
    }
    Ok(())
}

pub(super) fn contains_embedded_absolute_path(value: &str) -> bool {
    value.split_whitespace().any(|word| {
        let word = trim_token_punctuation(word);
        let scan = if let Some(marker) = word.find("://") {
            let scheme = &word[..marker];
            if !is_reference_scheme(scheme) {
                word
            } else if matches!(scheme, "http" | "https") || word == "artifact://engine/receipt" {
                return false;
            } else {
                let after_scheme = &word[marker + 3..];
                let Some(boundary) = after_scheme.find(|character: char| {
                    matches!(
                        character,
                        '/' | '?'
                            | '|'
                            | '='
                            | ','
                            | ';'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '"'
                            | '\''
                            | '<'
                            | '>'
                    )
                }) else {
                    return false;
                };
                &after_scheme[boundary..]
            }
        } else {
            word
        };

        scan.split(|character: char| {
            matches!(
                character,
                '=' | ':'
                    | '?'
                    | '|'
                    | ','
                    | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '"'
                    | '\''
                    | '<'
                    | '>'
            )
        })
        .map(trim_token_punctuation)
        .filter(|token| !token.is_empty())
        .any(|token| {
            token.starts_with('/') || token.starts_with("~/") || is_windows_drive_path(token)
        })
    })
}

pub(super) fn is_reference_scheme(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

pub(super) fn is_absolute_path_token(token: &str) -> bool {
    let token = trim_token_punctuation(token);
    token.starts_with('/') || is_windows_drive_path(token)
}

pub(super) fn trim_token_punctuation(token: &str) -> &str {
    token.trim_matches(|character: char| {
        matches!(
            character,
            '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | '.'
        )
    })
}

pub(super) fn is_credential_shaped_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    let jwt = token.split('.').count() == 3
        && token
            .split('.')
            .all(|part| part.len() >= 4 && part.bytes().all(is_base64url_byte));
    let credential_url =
        (token.contains("://") || token.starts_with("http:") || token.starts_with("https:"))
            && token.contains('@');
    let aws_access_key = bytes.len() == 20
        && (token.starts_with("akia") || token.starts_with("asia") || token.starts_with("aida"))
        && bytes[4..].iter().all(u8::is_ascii_alphanumeric);
    let aws_secret_key = bytes.len() == 40
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        && token.bytes().any(|byte| matches!(byte, b'+' | b'/'))
        && token.bytes().any(|byte| byte.is_ascii_uppercase())
        && token.bytes().any(|byte| byte.is_ascii_lowercase())
        && token.bytes().any(|byte| byte.is_ascii_digit());
    jwt || credential_url || aws_access_key || aws_secret_key
}

pub(super) fn contains_spaced_sensitive_assignment(value: &str) -> bool {
    const NAMES: [&str; 12] = [
        "token",
        "password",
        "passwd",
        "api_key",
        "access_key",
        "aws_secret",
        "client_secret",
        "key",
        "bearer",
        "x-api-key",
        "jwt",
        "url",
    ];
    NAMES.iter().any(|name| {
        value.match_indices(name).any(|(offset, _)| {
            let before = &value[..offset];
            let after = &value[offset + name.len()..];
            let has_boundary = before
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_ascii_alphanumeric() && character != '_');
            let after = after.trim_start();
            has_boundary
                && matches!(after.as_bytes().first(), Some(b'=' | b':'))
                && !after[1..].trim_start().is_empty()
        })
    })
}

pub(super) fn is_sensitive_assignment(token: &str) -> bool {
    token.starts_with("key=")
        || token == "key:"
        || token.starts_with("bearer=")
        || token == "bearer:"
        || token.starts_with("x-api-key=")
        || token == "x-api-key:"
}

pub(super) fn is_safe_hierarchical_reference(value: &str, scheme: &str, rest: &str) -> bool {
    value == "artifact://engine/receipt" && scheme == "artifact" && rest == "//engine/receipt"
}

pub(super) fn is_base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

pub(crate) fn is_unicode_format_character(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

pub(super) fn is_windows_drive_path(lowered: &str) -> bool {
    let bytes = lowered.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

pub(crate) fn canonical_bytes_of<T: Serialize>(
    value: &T,
    label: &str,
) -> Result<Vec<u8>, ValidationError> {
    let value = serde_json::to_value(value)
        .map_err(|error| ValidationError::new(format!("serialize {label}: {error}")))?;
    serde_json::to_vec(&sort_json(value))
        .map_err(|error| ValidationError::new(format!("canonicalize {label}: {error}")))
}

pub(super) fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        scalar => scalar,
    }
}

pub(crate) fn digest_with_domain(
    domain: &[u8],
    bytes: &[u8],
) -> Result<Sha256Digest, ValidationError> {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(encoded, "{byte:02x}")
            .map_err(|error| ValidationError::new(format!("encode checkpoint digest: {error}")))?;
    }
    Sha256Digest::new(encoded)
}
