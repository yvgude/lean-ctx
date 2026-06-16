macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

pub fn redaction_enabled_for_active_role() -> bool {
    let role = crate::core::roles::active_role();
    if role.role.name == "admin" {
        role.io.redact_outputs
    } else {
        // Contract: redaction never disabled for non-admin roles.
        true
    }
}

pub fn redact_text_if_enabled(input: &str) -> String {
    if !redaction_enabled_for_active_role() {
        return input.to_string();
    }
    redact_text(input)
}

/// Right-hand sides that look like `key: value` but are obviously not secrets:
/// TypeScript type annotations and language literals. Redacting these corrupts
/// source files read through `ctx_read` (GH #430), so the key/value rules skip
/// them. Compared case-insensitively after trimming surrounding quotes.
fn is_non_secret_literal(value: &str) -> bool {
    let v = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    matches!(
        v.to_ascii_lowercase().as_str(),
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

/// One redaction rule: a labelled regex plus how the match is rebuilt.
struct Rule {
    label: &'static str,
    re: &'static regex::Regex,
    /// When set, group 1 is a prefix to keep and group 2 is the secret value;
    /// the match is left untouched if that value is a non-secret literal
    /// (`password: undefined`, `secret: string`, …) — see `is_non_secret_literal`.
    guard_value: bool,
}

/// The single source of truth for secret patterns. `shell::redact` delegates
/// here so the two layers can never drift apart again.
fn redaction_rules() -> Vec<Rule> {
    vec![
        Rule {
            label: "Bearer token",
            re: static_regex!(r"(?i)(bearer\s+)[a-zA-Z0-9\-_\.]{8,}"),
            guard_value: false,
        },
        Rule {
            label: "Authorization header",
            re: static_regex!(r"(?i)(authorization:\s*(?:basic|bearer|token)\s+)[^\s\r\n]+"),
            guard_value: false,
        },
        // Key/value secrets: group 1 = `name=`/`name: ` prefix (kept), group 2 =
        // the value (redacted unless it is a non-secret literal — GH #430).
        Rule {
            label: "API key param",
            re: static_regex!(
                r#"(?i)((?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|token|password|passwd|pwd|secret)\s*[=:]\s*)([^\s\r\n,;&"']+)"#
            ),
            guard_value: true,
        },
        // Whole token is the secret — no prefix group, so the entire match is
        // replaced. (Previously group 1 captured the key itself and leaked it.)
        Rule {
            label: "AWS key",
            re: static_regex!(r"AKIA[0-9A-Z]{12,}"),
            guard_value: false,
        },
        Rule {
            label: "Private key block",
            re: static_regex!(
                r"(?s)(-----BEGIN\s+(?:RSA\s+)?PRIVATE\s+KEY-----).+?-----END\s+(?:RSA\s+)?PRIVATE\s+KEY-----"
            ),
            guard_value: false,
        },
        Rule {
            label: "GitHub token",
            re: static_regex!(r"(gh[pousr]_)[a-zA-Z0-9]{20,}"),
            guard_value: false,
        },
        // Group 1 = prefix (kept); the 32+ char value after it is redacted.
        // (Previously the value was captured into group 1 and kept verbatim.)
        Rule {
            label: "Generic long secret",
            re: static_regex!(
                r#"(?i)((?:key|token|secret|password|credential|auth)\s*[=:]\s*)['"]?[a-zA-Z0-9+/=\-_]{32,}['"]?"#
            ),
            guard_value: false,
        },
    ]
}

pub fn redact_text(input: &str) -> String {
    let mut out = input.to_string();
    for rule in redaction_rules() {
        out = rule
            .re
            .replace_all(&out, |caps: &regex::Captures| {
                if rule.guard_value
                    && let Some(value) = caps.get(2)
                    && is_non_secret_literal(value.as_str())
                {
                    // Not a secret (e.g. `password: undefined`) — keep verbatim.
                    return caps
                        .get(0)
                        .map_or(String::new(), |m| m.as_str().to_string());
                }
                match caps.get(1) {
                    Some(prefix) => format!("{}[REDACTED:{}]", prefix.as_str(), rule.label),
                    None => format!("[REDACTED:{}]", rule.label),
                }
            })
            .to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_token() {
        let s = "Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345";
        let out = redact_text(s);
        assert!(out.contains("[REDACTED"));
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn redacts_private_key_block() {
        let s = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----";
        let out = redact_text(s);
        assert!(out.contains("[REDACTED"));
        assert!(!out.contains("\nabc\n"));
    }

    #[test]
    fn redacts_api_key_param_value() {
        let out = redact_text("password=hunter2-super-secret-value");
        assert!(
            out.contains("password=[REDACTED:API key param]"),
            "got: {out}"
        );
        assert!(!out.contains("hunter2"));
    }

    /// GH #430: TypeScript type annotations and language literals must NOT be
    /// redacted — over-eager masking corrupted source files read via ctx_read.
    #[test]
    fn keeps_non_secret_literals() {
        for s in [
            "password: undefined",
            "secret: string",
            "token: null",
            "apiKey: boolean",
            "password = false",
            "secret: any",
            "let pwd: number = 1",
        ] {
            assert_eq!(redact_text(s), s, "must not redact non-secret literal: {s}");
        }
    }

    /// Whole-token secrets must be removed, not annotated in place — previously
    /// the closure kept group 1 (the key itself) and only appended `[REDACTED]`.
    #[test]
    fn fully_redacts_aws_key() {
        let out = redact_text("AKIAIOSFODNN7EXAMPLE");
        assert!(
            !out.contains("AKIAIOSFODNN7EXAMPLE"),
            "AWS key leaked: {out}"
        );
        assert!(out.contains("[REDACTED:AWS key]"));
    }

    #[test]
    fn fully_redacts_generic_long_secret() {
        // `credential=` is not covered by the API-key-param rule, so this
        // exercises the generic fallback (the previously leaky path).
        let secret = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6"; // 32 chars
        let out = redact_text(&format!("credential={secret}"));
        assert!(!out.contains(secret), "long secret leaked: {out}");
        assert!(
            out.contains("credential=[REDACTED:Generic long secret]"),
            "got: {out}"
        );
    }

    #[test]
    fn redacts_github_token_keeping_prefix() {
        let out = redact_text("ghp_abcdefghijklmnopqrstuvwxyz0123");
        assert!(out.starts_with("ghp_[REDACTED:GitHub token]"), "got: {out}");
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz"));
    }
}
