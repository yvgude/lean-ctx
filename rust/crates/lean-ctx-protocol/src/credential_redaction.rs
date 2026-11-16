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
    is_dotted_identifier(value)
}

fn is_dotted_identifier(value: &str) -> bool {
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

/// #2255: whether `value` reads as a nullable type name (`Session?`,
/// `java.time.Instant?`): an identifier chain ending in `?` whose last segment
/// starts upper-case. Cheap; callers test it before scanning back for the line.
#[must_use]
pub fn is_nullable_type_name(value: &str) -> bool {
    value.trim().strip_suffix('?').is_some_and(|name| {
        !name.is_empty()
            && is_dotted_identifier(name)
            && name.rsplit('.').next().is_some_and(|last| {
                last.starts_with(|character: char| character.is_ascii_uppercase())
            })
    })
}

/// #2255: whether `value` is a nullable type in a declaration
/// (`private var token: ProbeCorrelation? = null`, `fun f(token: Session?)`).
///
/// `line_before_value` is the text from the line start up to the value. The
/// value must be a type name ending in `?` (last segment starts upper-case),
/// and the line must start by declaring it: `val`/`var`/`let` (after optional
/// annotations and modifiers), or a `fun name(`/`func name(`/`init(`/
/// `constructor(` parameter list. A `.env`, YAML, URL or prose line does not,
/// so `password: Welcome?` stays a secret.
#[must_use]
pub fn is_nullable_type_annotation(line_before_value: &str, value: &str) -> bool {
    if !is_nullable_type_name(value) {
        return false;
    }
    // Anchored at the line start: modifier words like `static` or `open` are
    // English too, so a modifier elsewhere in a line declares nothing.
    let property = static_regex!(
        r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:private|public|internal|protected|override|open|final|static|weak|unowned|lateinit)\s+)*(?:val|var|let)\s+[A-Za-z_]\w*\s*:\s*$"
    );
    let parameter = static_regex!(
        r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:private|public|internal|protected|override|open|final|static|suspend|inline|operator|infix|mutating|convenience|required)\s+)*(?:func?\s+[\w.<>]+\s*|init\s*|constructor\s*)\((?:[^=()]*,)?\s*[A-Za-z_]\w*\s*:\s*$"
    );
    property.is_match(line_before_value) || parameter.is_match(line_before_value)
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
        let text = output.as_str();
        output = rule
            .regex
            .replace_all(text, |captures: &regex::Captures| {
                let whole = captures.get(0).map_or("", |value| value.as_str());
                if rule.guard_value
                    && let Some(value) = captures.get(2)
                {
                    if is_benign_secret_value(value.as_str()) {
                        return whole.to_owned();
                    }
                    // The line scan runs only for type-shaped `?` values.
                    if is_nullable_type_name(value.as_str()) {
                        let line_start = text[..value.start()].rfind('\n').map_or(0, |at| at + 1);
                        if is_nullable_type_annotation(
                            &text[line_start..value.start()],
                            value.as_str(),
                        ) {
                            return whole.to_owned();
                        }
                    }
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
            "private var token: ProbeCorrelation? = null",
            "safe source text",
        ] {
            assert!(!contains_credential(value), "{value}");
        }
        // A `?` value outside a declaration is a password, not a type (#2255).
        for value in [
            "password: Sunshine?",
            "DB_PASSWORD=Sunshine?",
            "https://h/cb?token=Letmein?&x=1",
            "  password: Welcome?",
            "The static password: Summer2024?",
            "- final password: Winter2025?",
            "have fun (password: Hunter22?, x)",
            "Calling init(admin, password: Summer2024?, x)",
        ] {
            assert!(contains_credential(value), "{value}");
        }
        assert!(!contains_credential(
            "fun open(token: Session?, password: Secret?)"
        ));
    }
}
