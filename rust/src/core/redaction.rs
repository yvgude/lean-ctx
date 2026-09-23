macro_rules! static_regex {
    ($pattern:expr_2021) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

pub(crate) fn redaction_enabled_for_active_role() -> bool {
    let role = crate::core::roles::active_role();
    if role.role.name == "admin" {
        role.io.redact_outputs
    } else {
        // Contract: redaction never disabled for non-admin roles.
        true
    }
}

pub(crate) fn redact_text_if_enabled(input: &str) -> String {
    if !redaction_enabled_for_active_role() {
        return input.to_string();
    }
    redact_text_with_excludes(input, config_exclude_patterns().as_slice())
}

/// #718: unquoted identifier or property-access chains (`SvelteKit`,
/// `inputEnv.POCKETBASE_SUPERUSER_PASSWORD`, `serverEnv.getStripeSecretKey`,
/// `confirmRequiredEndpointKeySchema`) are code REFERENCES to a secret, never
/// the literal value — the value lives in a gitignored `.env`. Digits make a
/// token secret-shaped (base64/hex), so any digit keeps the redaction
/// (conservative: `password=hunter2` stays covered).
/// #827: detect pure numeric values — integers, floats, scientific notation.
/// These are never secrets even when the key contains `token` or `key`.
fn looks_like_number(v: &str) -> bool {
    if v.is_empty() {
        return false;
    }
    let s = v.trim_start_matches(['+', '-']);
    if s.is_empty() {
        return false;
    }
    // Integer, float, or scientific notation (1.4e-06, 0.5, 600, 1e10)
    s.parse::<f64>().is_ok()
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-')
}

/// #827: env-variable reference patterns that should not be redacted.
/// Matches: `os.environ/NAME`, `os.getenv("NAME")`, `process.env.NAME`,
/// `inputEnv.NAME`, `${NAME}`, `$NAME`, `%NAME%` (Windows).
fn is_env_reference(v: &str) -> bool {
    if v.starts_with("os.environ/")
        || v.starts_with("os.getenv(")
        || v.starts_with("process.env.")
        || v.starts_with("System.getenv(")
        || v.starts_with("ENV[")
        || v.starts_with("env(")
    {
        return true;
    }
    // Variable interpolation: ${VAR}, $VAR, %VAR%
    if (v.starts_with("${") && v.ends_with('}'))
        || (v.starts_with('$')
            && v[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_'))
        || (v.starts_with('%') && v.ends_with('%') && v.len() > 2)
    {
        return true;
    }
    // Dotted identifier chains with an env-like prefix
    if let Some(prefix) = v.split('.').next() {
        let pl = prefix.to_ascii_lowercase();
        if matches!(
            pl.as_str(),
            "env" | "inputenv" | "serverenv" | "secrets" | "vars" | "environ"
        ) && v.contains('.')
        {
            return true;
        }
    }
    false
}

fn is_identifier_reference(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with('"') || v.starts_with('\'') || v.starts_with('`') {
        return false;
    }
    // #827: env-variable reference patterns are not secrets.
    // `os.environ/MY_SERVICE_API_KEY`, `process.env.NAME`, `${VAR}`, `$VAR`.
    if is_env_reference(v) {
        return true;
    }
    if v.contains(|c: char| c.is_ascii_digit()) {
        return false;
    }
    v.split('.').all(|segment| {
        let mut chars = segment.chars();
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
    })
}

/// #718: obvious provider-token placeholders, `your_key_here`,
/// `<insert-token>`) are documentation, not secrets — `.env.example` files
/// must survive ctx_read verbatim. #1831: so is an asterisk mask (`*****`),
/// which is what a redactor itself writes in place of a secret.
fn is_placeholder_value(value: &str) -> bool {
    let v = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_ascii_lowercase();
    if v.starts_with('<') && v.ends_with('>') {
        return true;
    }
    if !v.is_empty() && v.chars().all(|c| c == '*') {
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
    MARKERS.iter().any(|m| v.contains(m))
}

/// Right-hand sides that look like `key: value` but are obviously not secrets:
/// TypeScript type annotations and language literals. Redacting these corrupts
/// source files read through `ctx_read` (GH #430), so the key/value rules skip
/// them. Compared case-insensitively after trimming surrounding quotes.
fn is_non_secret_literal(value: &str) -> bool {
    let v = value
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    // #827: pure numbers (integer, float, scientific notation) are never secrets.
    // `input_cost_per_token: 1.4e-06` must not be redacted.
    if looks_like_number(v) {
        return true;
    }
    // Type expressions are never flat secret tokens: real keys/tokens are drawn
    // from `[A-Za-z0-9+/=_-]`, whereas type annotations carry angle brackets,
    // unions, arrays or call/object syntax. `password: Promise<string>` and
    // `apiKey: Record<string, unknown>` must survive ctx_read verbatim (GH #430).
    if v.contains(['<', '>', '|', '(', ')', '[', ']', '{', '}']) {
        return true;
    }
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
    /// (`password: undefined`), an identifier reference
    /// (`serverEnv.getStripeSecretKey`) or a provider-token placeholder —
    /// see `is_benign_secret_value` (GH #430, #718).
    guard_value: bool,
}

/// Combined benign-value check for key/value secret rules (#430 + #718):
/// language literals and type annotations, unquoted identifier/property
/// references, and documentation placeholders are never redacted. Quoted
/// string values stay protected — they ARE literal values.
pub(crate) fn is_benign_secret_value(value: &str) -> bool {
    is_comparison_operator_residue(value)
        || is_non_secret_literal(value)
        || is_identifier_reference(value)
        || is_placeholder_value(value)
}

/// #1095: when the regex `key=` consumes one `=` from `==`, the captured
/// "value" is a bare `=` (or `==`, `!=`). A comparison operator is never
/// a secret; redacting it corrupts source semantics.
fn is_comparison_operator_residue(value: &str) -> bool {
    let v = value.trim();
    matches!(v, "=" | "==" | "!=" | "<=" | ">=" | "===" | "!==")
}

/// The single source of truth for secret patterns. `shell::redact` delegates
/// here so the two layers can never drift apart again.
///
/// #718 word boundaries: the key/value alternations start with
/// `(?:^|[^a-z0-9])` (consumed into the kept prefix — the regex crate has no
/// lookbehind) so camelCase subwords (`superuserPassword`,
/// `getStripeSecretKey`) never trigger a rule, while SNAKE_CASE env names
/// (`GITHUB_FEEDBACK_TOKEN`) still do: `_` remains a permitted predecessor.
fn redaction_rules() -> Vec<Rule> {
    vec![
        Rule {
            label: "Bearer token",
            re: static_regex!(r"(?i)(bearer[ \t]+)[a-zA-Z0-9\-_\.]{8,}"),
            guard_value: false,
        },
        Rule {
            label: "Authorization header",
            re: static_regex!(r"(?i)(authorization:[ \t]*(?:basic|bearer|token)[ \t]+)[^\s\r\n]+"),
            guard_value: false,
        },
        // Key/value secrets: group 1 = predecessor + `name=`/`name: ` prefix
        // (kept), group 2 = the value (redacted unless benign — GH #430/#718).
        // #1830: only blanks around the separator, never a line break — a
        // valueless `token:` must not take the next line's key or diff marker
        // for its value.
        Rule {
            label: "API key param",
            re: static_regex!(
                r#"(?im)((?:^|[^a-z0-9])(?:api[_-]?key|apikey|access[_-]?key|secret[_-]?key|token|password|passwd|pwd|secret)[ \t]*[=:][ \t]*)([^\s\r\n,;&"']+)"#
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
        // Group 1 = prefix (kept), group 2 = the 32+ char value. Guarded since
        // #718: 32-char identifiers like `confirmRequiredEndpointKeySchema`
        // are references, not secrets.
        Rule {
            label: "Generic long secret",
            re: static_regex!(
                r#"(?im)((?:^|[^a-z0-9])(?:key|token|secret|password|credential|auth)[ \t]*[=:][ \t]*)(['"]?[a-zA-Z0-9+/=\-_]{32,}['"]?)"#
            ),
            guard_value: true,
        },
    ]
}

pub(crate) fn redact_text(input: &str) -> String {
    redact_text_with_excludes(input, &[])
}

/// #718: `redact_text` with subtractive user patterns from
/// `[secret_detection].exclude_patterns` — a match covered by any exclude
/// regex is kept verbatim, so known-safe naming conventions can be carved out
/// without disabling secret detection wholesale.
pub(crate) fn redact_text_with_excludes(input: &str, excludes: &[regex::Regex]) -> String {
    let mut out = input.to_string();
    for rule in redaction_rules() {
        out = rule
            .re
            .replace_all(&out, |caps: &regex::Captures| {
                let whole = caps.get(0).map_or("", |m| m.as_str());
                if excludes.iter().any(|ex| ex.is_match(whole)) {
                    return whole.to_string();
                }
                if rule.guard_value
                    && let Some(value) = caps.get(2)
                    && is_benign_secret_value(value.as_str())
                {
                    // Not a secret (identifier reference, literal, placeholder)
                    // — keep verbatim (#430, #718).
                    return whole.to_string();
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

/// Compile the configured `exclude_patterns` (#718). Invalid regexes are
/// skipped — a broken exclude must never disable redaction.
///
/// #952: regex compilation — the expensive part of this call — used to run
/// on every redaction call (`ctx_read`, `ctx_shell`, `ctx_execute` all funnel
/// through `redact_text_if_enabled`) regardless of whether the config had
/// changed. Cached here, keyed on `Arc::ptr_eq` against the config `Arc`:
/// `Config::load_arc` returns the *same* `Arc` when the underlying config
/// content hash is unchanged, so a real config edit is exactly what
/// invalidates this cache too — no separate change-detection needed.
pub(crate) fn config_exclude_patterns() -> std::sync::Arc<Vec<regex::Regex>> {
    type Cache = Option<(
        std::sync::Arc<crate::core::config::Config>,
        std::sync::Arc<Vec<regex::Regex>>,
    )>;
    static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(None);

    let cfg = crate::core::config::Config::load_arc();

    if let Ok(guard) = CACHE.lock()
        && let Some((cached_cfg, patterns)) = &*guard
        && std::sync::Arc::ptr_eq(cached_cfg, &cfg)
    {
        return std::sync::Arc::clone(patterns);
    }

    let compiled = std::sync::Arc::new(
        cfg.secret_detection
            .exclude_patterns
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect::<Vec<_>>(),
    );

    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((cfg, std::sync::Arc::clone(&compiled)));
    }

    compiled
}

/// Apply caller-supplied policy redaction patterns on top of the built-in
/// secret rules: each regex match becomes `[REDACTED:<label>]`. Returns the
/// transformed text and the number of redactions applied (for audit counts).
///
/// Used by context policy packs (GL #673) so a pack's `[redaction]` block
/// actually removes matching content from what the model sees. The patterns are
/// the pack's `[redaction]` entries, precompiled by
/// [`crate::core::policy::runtime`].
#[must_use]
pub(crate) fn redact_with_patterns(
    input: &str,
    patterns: &[(String, regex::Regex)],
) -> (String, usize) {
    let mut out = input.to_string();
    let mut hits = 0usize;
    for (label, re) in patterns {
        let mut local = 0usize;
        out = re
            .replace_all(&out, |_caps: &regex::Captures| {
                local += 1;
                format!("[REDACTED:{label}]")
            })
            .to_string();
        hits += local;
    }
    (out, hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- #952: exclude_patterns compilation cache ---

    #[test]
    fn config_exclude_patterns_reuses_compiled_regexes_when_config_unchanged() {
        let first = config_exclude_patterns();
        let second = config_exclude_patterns();
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "unchanged config must reuse the cached compiled patterns, not recompile"
        );
    }

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

    /// GH #430: TS type annotations (generics, unions, arrays, function/object
    /// types) carry angle brackets / brackets that real secret tokens never do,
    /// so they must survive verbatim even when the key looks sensitive.
    #[test]
    fn keeps_type_annotations() {
        for s in [
            "password: Promise<string>",
            "apiKey: Record<string, unknown>",
            "token: string[]",
            "secret: () => void",
            "password: string | undefined",
            "credential: { value: string }",
        ] {
            assert_eq!(redact_text(s), s, "must not redact type annotation: {s}");
        }
    }

    /// Whole-token secrets must be removed, not annotated in place — previously
    /// the closure kept group 1 (the key itself) and only appended `[REDACTED]`.
    #[test]
    fn fully_redacts_aws_key() {
        let aws_key = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let out = redact_text(aws_key);
        assert!(!out.contains(aws_key), "AWS key leaked: {out}");
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
        let github_token = concat!("gh", "p_", "abcdefghijklmnopqrstuvwxyz0123");
        let out = redact_text(github_token);
        assert!(
            out.starts_with(concat!("gh", "p_", "[REDACTED:GitHub token]")),
            "got: {out}"
        );
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz"));
    }

    // ── #718: benign identifier references, prose and placeholders ──

    /// Repro 1: prose that mentions a keyword must not have the following
    /// word redacted — "token: SvelteKit's…" is documentation, not a secret.
    #[test]
    fn keeps_prose_identifier_after_keyword() {
        let s = "the CSRF token: SvelteKit's native origin-check on form actions";
        assert_eq!(redact_text(s), s, "prose must survive verbatim");
    }

    /// Repro 2: camelCase subwords must not trigger the keyword alternation,
    /// and identifier/property-access RHS values are references, not secrets.
    #[test]
    fn keeps_identifier_and_property_references() {
        for s in [
            "superuserPassword: inputEnv.POCKETBASE_SUPERUSER_PASSWORD",
            "export const getStripeSecretKey = serverEnv.getStripeSecretKey;",
            "const apiKey = config.stripeApiKey",
        ] {
            assert_eq!(redact_text(s), s, "identifier reference redacted: {s}");
        }
    }

    /// Repro 3: a 32+ char identifier (Zod schema name) is a reference —
    /// "Generic long secret" needs the same value guard as the API-key rule.
    #[test]
    fn keeps_long_schema_identifier() {
        let s = "endpoint_key: confirmRequiredEndpointKeySchema,";
        assert_eq!(redact_text(s), s, "schema identifier must not be redacted");
    }

    /// Repro 4: obvious placeholder values (.env.example) are documentation.
    #[test]
    fn keeps_placeholder_values() {
        for s in [
            concat!("GITHUB_FEEDBACK_TOKEN=gh", "p_change_me"),
            "API_KEY=your_key_here",
            "password=<insert-password>",
            "SECRET_KEY=xxxxxxxx",
        ] {
            assert_eq!(redact_text(s), s, "placeholder redacted: {s}");
        }
    }

    /// #1830: a keyword with no value on its line must not take the next
    /// line's diff marker or key for its value.
    #[test]
    fn a_valueless_key_does_not_reach_into_the_next_line() {
        for s in [
            "+token:\n+  accesstoken: acc",
            "token:\n  refreshtoken: ref",
            "password =\n-  old_value",
            "Authorization: Bearer\nnext_line_word",
            "credential:\n  ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef012345",
        ] {
            assert_eq!(redact_text(s), s, "crossed a line break: {s:?}");
        }
        // The value on the key's own line is still redacted.
        let out = redact_text("token:\n  password: hunter2-real-value");
        assert!(!out.contains("hunter2"), "leaked: {out}");
        assert!(
            out.starts_with("token:\n  password: [REDACTED"),
            "got: {out}"
        );
    }

    /// #1831: an asterisk mask is what a redactor writes; it is not a secret,
    /// and a `full` read must round-trip it into an edit.
    #[test]
    fn keeps_asterisk_masks() {
        for s in [
            "password: *****",
            "password: ***",
            r#"expected := []string{"password: *****", "title: My Home"}"#,
            "token=*",
        ] {
            assert_eq!(redact_text(s), s, "mask redacted: {s}");
        }
        // A value that merely contains asterisks is not a mask.
        assert!(redact_text("password: ab*cd1234").contains("[REDACTED"));
    }

    /// The flip side: real secret-shaped values must STILL be redacted after
    /// the #718 guards.
    #[test]
    fn still_redacts_real_secret_values() {
        // Digit-bearing value after a snake_case env name.
        let out = redact_text("GITHUB_TOKEN=ghpA1b2c3d4e5f6g7h8");
        assert!(!out.contains("ghpA1b2c3d4e5f6g7h8"), "leaked: {out}");
        // SNAKE_CASE env assignment with digits (the _ predecessor stays a
        // word boundary that MATCHES).
        let out = redact_text("MY_SECRET=abc123def456ghi789");
        assert!(!out.contains("abc123def456ghi789"), "leaked: {out}");
        // Quoted 32+ char literal: a quoted value is never an identifier
        // reference, so the Generic-long-secret guard keeps redacting it.
        let quoted = "key: 'abcdefghijklmnopqrstuvwxyzabcdef'";
        let out = redact_text(quoted);
        assert!(
            !out.contains("abcdefghijklmnopqrstuvwxyzabcdef"),
            "leaked: {out}"
        );
    }

    /// #718: exclude_patterns carve matches out subtractively.
    #[test]
    fn exclude_patterns_skip_matching_redactions() {
        let excludes = vec![regex::Regex::new(r"LCTX_TEST_\w+").unwrap()];
        let input = "token=LCTX_TEST_a1b2c3d4e5";
        assert_eq!(
            redact_text_with_excludes(input, &excludes),
            input,
            "excluded match must stay verbatim"
        );
        // Without the exclude the same value IS redacted (digits → secret).
        assert!(redact_text(input).contains("[REDACTED"));
    }

    #[test]
    fn identifier_and_placeholder_heuristics() {
        assert!(is_identifier_reference("serverEnv.getStripeSecretKey"));
        assert!(is_identifier_reference("confirmRequiredEndpointKeySchema"));
        assert!(is_identifier_reference("$scope._private"));
        assert!(!is_identifier_reference("abc123"), "digits → secret-shaped");
        assert!(!is_identifier_reference("\"quoted\""), "literal value");
        assert!(!is_identifier_reference("a-b"), "dash is not identifier");
        assert!(is_placeholder_value(concat!("gh", "p_change_me")));
        assert!(is_placeholder_value("<token>"));
        assert!(is_placeholder_value("your_api_key_123"));
        assert!(!is_placeholder_value("A1b2C3d4E5f6G7h8"));
    }

    #[test]
    fn policy_patterns_redact_with_label_and_count() {
        let patterns = vec![(
            "employee_id".to_string(),
            regex::Regex::new(r"EMP-\d{4}").unwrap(),
        )];
        let (out, hits) = redact_with_patterns("user EMP-1234 and EMP-5678", &patterns);
        assert_eq!(hits, 2);
        assert!(!out.contains("EMP-1234"));
        assert!(out.contains("[REDACTED:employee_id]"));
    }

    #[test]
    fn policy_patterns_noop_when_no_match() {
        let patterns = vec![("iban".to_string(), regex::Regex::new(r"CH\d{2}").unwrap())];
        let (out, hits) = redact_with_patterns("nothing sensitive here", &patterns);
        assert_eq!(hits, 0);
        assert_eq!(out, "nothing sensitive here");
    }

    /// GH #827: scientific notation and plain numbers after keys containing
    /// `token` must not be redacted — these are pricing/cost fields.
    #[test]
    fn keeps_numeric_values_827() {
        for (input, desc) in [
            ("input_cost_per_token: 1.4e-06", "scientific notation"),
            ("output_cost_per_token: 4.4e-06", "scientific notation"),
            (
                "cache_read_input_token_cost: 1.9e-07",
                "scientific notation",
            ),
            ("token: 600", "plain integer"),
            ("secret: 0.5", "decimal float"),
            ("api_key: 42", "small integer"),
            ("password: 3.14", "pi float"),
        ] {
            assert_eq!(
                redact_text(input),
                input,
                "must not redact numeric value ({desc}): {input}"
            );
        }
    }

    /// GH #827: env-variable references must not be redacted — they are
    /// pointers to secrets, not the secrets themselves.
    #[test]
    fn keeps_env_references_827() {
        for (input, desc) in [
            (
                "api_key: os.environ/MY_SERVICE_API_KEY",
                "Python os.environ/",
            ),
            ("secret: os.getenv(MY_KEY)", "Python os.getenv()"),
            ("token: process.env.API_TOKEN", "Node process.env"),
            (
                "password: inputEnv.POCKETBASE_SUPERUSER_PASSWORD",
                "inputEnv dot ref",
            ),
            ("api_key: ${MY_API_KEY}", "shell interpolation ${}"),
            ("secret: $MY_SECRET", "shell $VAR"),
            ("token: %API_TOKEN%", "Windows %VAR%"),
            ("api_key: ENV[API_KEY]", "Ruby ENV[]"),
            ("secret: env(SECRET_KEY)", "Laravel env()"),
            ("password: System.getenv(DB_PASS)", "Java System.getenv"),
        ] {
            assert_eq!(
                redact_text(input),
                input,
                "must not redact env ref ({desc}): {input}"
            );
        }
    }

    /// GH #827: real secrets must STILL be redacted (regression guard).
    #[test]
    fn still_redacts_real_secrets_827() {
        for input in [
            "api_key: sk-1234567890abcdef1234567890abcdef",
            "password: hunter2-super-secret-value",
            "token: eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload.signature",
        ] {
            let out = redact_text(input);
            assert!(
                out.contains("[REDACTED"),
                "must redact real secret: {input} -> {out}"
            );
        }
    }

    /// GH #1095: `token == ""` in Go source must not be redacted — the regex
    /// consumes the first `=` leaving group 2 = `=` (comparison residue).
    #[test]
    fn keeps_comparison_operators_in_source() {
        for s in [
            r#"if token == "" {"#,
            r#"if token, err = checkPulsares(); token == "" || err != nil {"#,
            r#"if password != "" {"#,
            r"assert secret == expected_value",
        ] {
            assert_eq!(redact_text(s), s, "comparison operator corrupted: {s}");
        }
    }
}
