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

/// #1302: whether a value holding bracket or union syntax reads as code (a
/// type, call, index or template) rather than a secret that merely contains
/// such a character. Code nests its brackets and puts an operator or another
/// bracket after a closer: `Promise<string>`, `getToken()`, `string[]`,
/// `os.getenv(`, `&[u8]`, `T|null`, `${{`, `map[string]string`. A secret does
/// not: `Pa(ss)word1` continues after `)`, `ab]c` closes what it never
/// opened, `p@ss[w0rd` is no identifier before its bracket. An opener left
/// open at the end is code (the value stops at the first blank or comma).
#[must_use]
pub fn is_code_shaped(value: &str) -> bool {
    // A bare `|` is a union or a cell separator (`KEEP_LEFT|secret=…|KEEP_RIGHT`):
    // judging the whole run would mask the neighbouring cell, so it stays
    // as before and the token detectors judge the secret itself.
    if !value.contains(['<', '>', '(', ')', '[', ']', '{', '}']) {
        return true;
    }
    // printf-style `%(name)s` and YAML block scalars `|`, `>-`, `|2`.
    if static_regex!(r"^(?:%\([A-Za-z_]\w*\)[sdifr]|[|>][+-]?[0-9]?)$").is_match(value) {
        return true;
    }
    let bytes = value.as_bytes();
    let mut at = 0;
    // Leading sigils: references, pointers and template markers.
    while at < bytes.len() && matches!(bytes[at], b'&' | b'*' | b'#' | b'$' | b'%' | b'@' | b'!') {
        at += 1;
    }
    // ERB/JSP `<%=`.
    if value[at..].starts_with("<%") {
        return true;
    }
    // Open groups: (opener, word before it, start of its content).
    let mut open: Vec<(u8, Option<&str>, usize)> = Vec::new();
    // After a closer: whether a word may follow it.
    let mut closed: Option<bool> = None;
    let mut word_before: Option<&str> = None;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' {
            let start = at;
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric()
                    || matches!(bytes[at], b'_' | b'$' | b'.' | b':'))
            {
                at += 1;
            }
            let word = &value[start..at];
            let numeric = word.bytes().all(|b| b.is_ascii_digit());
            if !numeric && byte.is_ascii_digit() {
                return false;
            }
            if closed
                .take()
                .is_some_and(|word_may_follow| !word_may_follow)
            {
                return false;
            }
            word_before = Some(word);
            continue;
        }
        let word = word_before.take();
        let after_closer = closed.take().is_some();
        match byte {
            // After a closer, `&` and `*` are password characters, and `!`
            // is code only as a trailing non-null assertion (`foo()!`,
            // `foo()!.bar`): `Pa(ss)!word1`, `Pa(ss)*w0rd`.
            b'&' | b'*' if after_closer => return false,
            b'!' if after_closer && !matches!(bytes.get(at + 1), None | Some(b'.')) => {
                return false;
            }
            b'(' | b'[' | b'{' | b'<' => open.push((byte, word, at + 1)),
            b'>' if at > 0 && matches!(bytes[at - 1], b'=' | b'-') => {}
            b')' | b']' | b'}' | b'>' => {
                let expected = match byte {
                    b')' => b'(',
                    b']' => b'[',
                    b'}' => b'{',
                    _ => b'<',
                };
                let Some((opener, before, content_start)) = open.pop() else {
                    return false;
                };
                if opener != expected {
                    return false;
                }
                let sized = value[content_start..at].bytes().all(|b| b.is_ascii_digit());
                // Only Go's `[]T`, `[4]T` and `map[K]V` continue with a word.
                closed =
                    Some(byte == b']' && ((sized && before.is_none()) || before == Some("map")));
            }
            b'|' | b'?' | b'!' | b'&' | b'*' | b'.' | b':' | b',' => {}
            b'=' | b'-' if bytes.get(at + 1) == Some(&b'>') => {}
            _ => return false,
        }
        at += 1;
    }
    true
}

fn is_non_secret_literal(value: &str) -> bool {
    let value = value
        .trim()
        .trim_matches(|character| character == '"' || character == '\'' || character == '`');
    if looks_like_number(value) {
        return true;
    }
    if value.contains(['<', '>', '|', '(', ')', '[', ']', '{', '}']) {
        return is_code_shaped(value);
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
    ) || is_primitive_type_name(value)
}

/// Primitive type names (the list core redaction keeps) of Rust, Go, Java and C-family languages. In
/// `const MAX_TOKEN: usize = 4` the "value" after `TOKEN:` is a type
/// annotation, never a credential; masking it corrupts every read of such a
/// source. Matched exactly (case-sensitive), so a password that merely
/// contains one of these words is still protected.
fn is_primitive_type_name(value: &str) -> bool {
    matches!(
        value,
        "bool"
            | "char"
            | "str"
            | "usize"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
            | "float"
            | "float32"
            | "float64"
            | "double"
            | "long"
            | "short"
            | "byte"
            | "rune"
            | "error"
            | "Self"
    )
}

/// The value without the syntax that follows it in an expression: a trailing
/// `:` and every unbalanced closing `)`, `]` or `}` — the reduction core
/// redaction judges with (`canonical_secret_value`). The value of
/// `(password: <x>?)` is captured as `<x>?)`; judged as captured, the `)` read
/// as type syntax and the password stayed visible (#2255).
fn canonical_secret_value(value: &str) -> &str {
    let mut value = value.trim();
    loop {
        if let Some(rest) = value.strip_suffix(':') {
            value = rest;
            continue;
        }
        let (open, close) = match value.chars().last() {
            Some(')') => ('(', ')'),
            Some(']') => ('[', ']'),
            Some('}') => ('{', '}'),
            _ => break,
        };
        if value.matches(open).count() >= value.matches(close).count() {
            break;
        }
        value = &value[..value.len() - 1];
    }
    value
}

fn is_benign_secret_value(value: &str) -> bool {
    let value = canonical_secret_value(value);
    matches!(value, "=" | "==" | "!=" | "<=" | ">=" | "===" | "!==")
        || is_non_secret_literal(value)
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
                    // The line scan runs only for type-shaped `?` values,
                    // judged without a closing `)` (`fun f(token: Session?)`).
                    let canonical = canonical_secret_value(value.as_str());
                    if is_nullable_type_name(canonical) {
                        let line_start = text[..value.start()].rfind('\n').map_or(0, |at| at + 1);
                        if is_nullable_type_annotation(&text[line_start..value.start()], canonical)
                        {
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
        // A closer after the value belongs to the expression, not the value.
        for value in [
            "f(password: Welcome?)",
            "{password: Welcome?}",
            "[pwd=s3cret]",
        ] {
            assert!(contains_credential(value), "{value}");
        }
        assert!(!contains_credential("f(token: Option<String>)"));
        // A bracket inside a value is code only when it reads as code (#1302).
        for value in [
            "password=Pa(ss)word1",
            "password=ab]c",
            "secret: p@ss[w0rd",
            "password=Pa(ss)!word1",
            "password=Pa(ss)*w0rd",
        ] {
            assert!(contains_credential(value), "{value}");
        }
        for value in [
            "token = getToken()",
            "password: Promise<string>",
            "secret map[string]string",
            "api_key: &[u8]",
            "token: string[]",
            "password: T|null",
            "token=${{ secrets.TOKEN }}",
            "password: %(db_password)s",
            "secret: |",
        ] {
            assert!(!contains_credential(value), "{value}");
        }
        assert!(!contains_credential(
            "fun open(token: Session?, password: Secret?)"
        ));
    }

    /// #2255: a value followed by a closing bracket (`(password: <x>?)`,
    /// `{token: <x>}`) was captured with the bracket, and the bracket read as
    /// type syntax, so the secret stayed visible. Unbalanced closers are now
    /// stripped before the judgement; a declared nullable type stays visible.
    #[test]
    fn a_secret_before_a_closing_bracket_is_masked() {
        // Built at run time: no fixture looks like a real secret to a scanner.
        let secret = ["Hun", "ter", "22"].concat();
        let word = ["Let", "me", "in"].concat();
        for line in [
            format!("have fun (password: {secret}?)"),
            format!("{{password: {secret}?}}"),
            format!("[password: {secret}?]"),
            format!("(password: {secret})"),
            format!("{{token: {secret}}}"),
            format!("call(api_key: {secret}))"),
            format!("f(token: {word}?)"),
        ] {
            let redacted = redact_text(&line);
            assert!(
                !redacted.contains(&secret) && !redacted.contains(&word),
                "{line} -> {redacted}"
            );
            assert!(redacted.contains("[REDACTED:"), "{line} -> {redacted}");
        }
        for kept in [
            "fun open(token: Session?)",
            "init(password: Secret?) {",
            "password: Promise<string>",
            "token: compute(x)",
            "(token: process.env.API_TOKEN)",
        ] {
            assert_eq!(redact_text(kept), kept);
        }
    }

    /// !427 review N1: with the closing bracket stripped, a primitive type
    /// annotation (`fn f(token: u64)`) was masked here while core redaction
    /// kept it; both layers now keep the same primitive type names.
    #[test]
    fn a_primitive_type_annotation_stays_readable() {
        for kept in [
            "fn f(token: u64)",
            "fn f(token: u64) -> u64 {",
            "fn f(secret: f64)",
        ] {
            assert_eq!(redact_text(kept), kept);
        }
        let secret = ["Hun", "ter", "22"].concat();
        let line = format!("fn f(token: {secret})");
        assert!(!redact_text(&line).contains(&secret));
    }
}
