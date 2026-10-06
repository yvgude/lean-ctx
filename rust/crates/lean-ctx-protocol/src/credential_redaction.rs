// SPDX-License-Identifier: Apache-2.0

//! Shared deterministic credential redaction used by Local and Via Edge.
//!
//! This is the provider-independent, configuration-free subset of LeanCTX's
//! established redaction policy. Callers that need configuration exclusions
//! layer those separately; security boundaries use this default-deny baseline.

use std::sync::OnceLock;

use regex::Regex;

macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new($pattern).expect("valid credential regex"))
    }};
}

fn looks_like_number(value: &str) -> bool {
    let value = value.trim_start_matches(['+', '-']);
    !value.is_empty()
        && value.parse::<f64>().is_ok()
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || ".eE+-".contains(character))
}

fn is_env_reference(value: &str) -> bool {
    if value.starts_with("os.environ/")
        || value.starts_with("os.getenv(")
        || value.starts_with("process.env.")
        || value.starts_with("System.getenv(")
        || value.starts_with("ENV[")
        || value.starts_with("env(")
    {
        return true;
    }
    if (value.starts_with("${") && value.ends_with('}'))
        || (value.starts_with('$')
            && value[1..]
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_'))
        || (value.starts_with('%') && value.ends_with('%') && value.len() > 2)
    {
        return true;
    }
    value.split('.').next().is_some_and(|prefix| {
        matches!(
            prefix.to_ascii_lowercase().as_str(),
            "env" | "inputenv" | "serverenv" | "secrets" | "vars" | "environ"
        ) && value.contains('.')
    })
}

fn is_identifier_reference(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() || value.starts_with(['"', '\'', '`']) {
        return false;
    }
    if is_env_reference(value) {
        return true;
    }
    if value.contains(|character: char| character.is_ascii_digit()) {
        return false;
    }
    value.split('.').all(|segment| {
        let mut characters = segment.chars();
        matches!(
            characters.next(),
            Some(character) if character.is_ascii_alphabetic() || character == '_' || character == '$'
        ) && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '$'
        })
    })
}

fn is_placeholder_value(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'' || character == '`')
        .to_ascii_lowercase();
    if value.starts_with('<') && value.ends_with('>') {
        return true;
    }
    const MARKERS: &[&str] = &[
        "change_me",
        "change-me",
        "changeme",
        "example",
        "placeholder",
        "your_",
        "your-",
        "xxx",
        "dummy",
        "sample",
        "todo",
        "fixme",
        "replace_me",
        "replace-me",
    ];
    MARKERS.iter().any(|marker| value.contains(marker))
}

fn is_non_secret_literal(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'' || character == '`');
    if looks_like_number(value) {
        return true;
    }
    if value.contains(['<', '>', '|', '(', ')', '[', ']', '{', '}']) {
        return true;
    }
    matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "undefined"
            | "null"
            | "none"
            | "nil"
            | "true"
            | "false"
            | "string"
            | "number"
            | "boolean"
            | "bigint"
            | "symbol"
            | "object"
            | "any"
            | "unknown"
            | "never"
            | "void"
            | "nan"
            | "date"
    )
}

fn is_benign_secret_value(value: &str) -> bool {
    matches!(
        value.trim(),
        "=" | "==" | "!=" | "<=" | ">=" | "===" | "!=="
    ) || is_non_secret_literal(value)
        || is_identifier_reference(value)
        || is_placeholder_value(value)
}

struct Rule {
    label: &'static str,
    regex: &'static Regex,
    guard_value: bool,
}

fn rules() -> Vec<Rule> {
    vec![
        Rule {
            label: "Bearer token",
            regex: static_regex!(r"(?i)(bearer\s+)[a-zA-Z0-9\-_\.]{8,}"),
            guard_value: false,
        },
        Rule {
            label: "Authorization header",
            regex: static_regex!(r"(?i)(authorization:\s*(?:basic|bearer|token)\s+)[^\s\r\n]+"),
            guard_value: false,
        },
        Rule {
            label: "API key param",
            regex: static_regex!(
                r#"(?im)((?:^|[^a-z0-9])(?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|token|password|passwd|pwd|secret)\s*[=:]\s*)([^\s\r\n,;&"']+)"#
            ),
            guard_value: true,
        },
        Rule {
            label: "AWS key",
            regex: static_regex!(r"AKIA[0-9A-Z]{12,}"),
            guard_value: false,
        },
        Rule {
            label: "Provider API key",
            regex: static_regex!(r"(?i)(?:sk-|AIza)[a-zA-Z0-9_\-]{8,}"),
            guard_value: false,
        },
        Rule {
            label: "Slack token",
            regex: static_regex!(r"(?i)xox[bpa]-[a-zA-Z0-9_\-]{8,}"),
            guard_value: false,
        },
        Rule {
            label: "Private key block",
            regex: static_regex!(
                r"(?s)(-----BEGIN\s+(?:RSA\s+)?PRIVATE\s+KEY-----).+?-----END\s+(?:RSA\s+)?PRIVATE\s+KEY-----"
            ),
            guard_value: false,
        },
        Rule {
            label: "GitHub token",
            regex: static_regex!(r"(gh[pousr]_)[a-zA-Z0-9]{20,}"),
            guard_value: false,
        },
        Rule {
            label: "Generic long secret",
            regex: static_regex!(
                r#"(?im)((?:^|[^a-z0-9])(?:key|token|secret|password|credential|auth)\s*[=:]\s*)(['"]?[a-zA-Z0-9+/=\-_]{32,}['"]?)"#
            ),
            guard_value: true,
        },
    ]
}

/// Apply the established, configuration-free LeanCTX credential rules.
#[must_use]
pub fn redact_text(input: &str) -> String {
    let mut output = input.to_owned();
    for rule in rules() {
        output = rule
            .regex
            .replace_all(&output, |captures: &regex::Captures| {
                let whole = captures.get(0).map_or("", |value| value.as_str());
                if rule.guard_value
                    && let Some(value) = captures.get(2)
                    && is_benign_secret_value(value.as_str())
                {
                    return whole.to_owned();
                }
                match captures.get(1) {
                    Some(prefix) => format!("{}[REDACTED:{}]", prefix.as_str(), rule.label),
                    None => format!("[REDACTED:{}]", rule.label),
                }
            })
            .into_owned();
    }
    output
}

/// Return true when the established rules would redact any credential material.
#[must_use]
pub fn contains_credential(input: &str) -> bool {
    redact_text(input) != input
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_provider_credentials_and_preserves_references() {
        for value in [
            "Authorization: Bearer secret-token-123",
            "api_key=sk-provider-secret-123456",
            "AKIA0123456789ABCDEF",
            "sk-provider-secret-123456",
            "AIza0123456789abcdefghijklmn",
            "xoxb-0123456789abcdefghijklmn",
            "ghp_0123456789abcdefghijklmn",
            "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----",
        ] {
            assert!(contains_credential(value), "{value}");
        }
        for value in [
            "api_key=${OPENAI_API_KEY}",
            "token: process.env.API_TOKEN",
            "password: undefined",
            "safe source text",
        ] {
            assert!(!contains_credential(value), "{value}");
        }
    }
}
