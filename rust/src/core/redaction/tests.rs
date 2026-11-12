// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A package manifest's verified signature key is kept in its own
/// member, as written or nested in an envelope; any other value there,
/// and any key outside the exemption, is still masked.
#[test]
fn a_verified_package_signer_key_is_not_masked() {
    let key = "ab".repeat(32);
    let plain = format!(r#"{{"signature":{{"public_key":"{key}"}}}}"#);
    let nested = serde_json::to_string(&plain).expect("nested");
    assert!(
        !redact_text(&plain).contains(&key),
        "masked without the exemption"
    );
    with_verified_signer_key(&key, || {
        assert_eq!(redact_text(&plain), plain);
        assert_eq!(redact_text(&nested), nested);
        let other = plain.replace(&key, &"cd".repeat(32));
        assert_ne!(redact_text(&other), other, "only the verified key");
    });
}

#[test]
fn standalone_provider_keys_are_fully_masked_in_nested_values() {
    let suffix = "aB3_".repeat(9);
    for prefix in ["sk-proj-", "sk-svcacct-", "sk-ant-"] {
        let secret = format!("{prefix}{suffix}");
        let raw = serde_json::json!({"nested":[secret.clone()]}).to_string();
        let safe = redact_text(&raw);
        assert!(!safe.contains(&suffix), "must mask the entire token");
        assert!(!safe.contains(prefix), "must not retain the secret prefix");
        assert!(safe.contains("REDACTED"));
        assert!(!crate::core::secret_detection::detect_secrets(&secret).is_empty());
    }
    assert_eq!(
        redact_text("sk-proj-your_key_here"),
        "sk-proj-your_key_here"
    );
}

// --- #952: exclude_patterns compilation cache ---

#[test]
fn config_exclude_patterns_reuses_compiled_regexes_when_config_unchanged() {
    // Other tests swap the config dir; "unchanged" needs the env lock.
    let _lock = crate::core::data_dir::test_env_lock();
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

/// A key cut off before its END marker — a line window, a truncated file,
/// a log tail — is masked through its last key-material line; text before
/// the header and code after the key material survive.
#[test]
fn redacts_a_truncated_private_key_but_not_the_code_around_it() {
    let s = concat!(
        "config:\n-----BEGIN RSA PRIVATE",
        " KEY-----\nProc-Type: 4,ENCRYPTED\nMIIEpAIBAAKCAQEA\nqx7Wmore"
    );
    let out = redact_text(s);
    assert!(out.starts_with("config:\n"), "{out}");
    assert!(out.contains("[REDACTED"), "{out}");
    for leaked in ["RSA PRIVATE", "Proc-Type", "MIIEpAIBAAKCAQEA", "qx7Wmore"] {
        assert!(!out.contains(leaked), "{leaked} leaked: {out}");
    }

    // A source that only names the header keeps the rest of the file.
    let source = concat!(
        "const HEADER: &str = \"-----BEGIN PRIVATE",
        " KEY-----\";\nfn parse(pem: &str) -> bool {\n    pem.starts_with(HEADER)\n}\n"
    );
    let out = redact_text(source);
    assert!(out.contains("fn parse(pem: &str) -> bool {"), "{out}");
    assert!(out.contains("pem.starts_with(HEADER)"), "{out}");
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

/// A primitive type after a sensitive-looking name is a declaration, not
/// a credential (`const ESTIMATED_CHARS_PER_TOKEN: u64 = 4;` was read
/// back as `[REDACTED:API key param] = 4`). A value that merely contains
/// a type name stays protected.
#[test]
fn keeps_primitive_type_declarations() {
    for s in [
        "const ESTIMATED_CHARS_PER_TOKEN: u64 = 4;",
        "const MAX_TOKEN: usize = 4;",
        "    max_token: u32,",
        "var Token int64",
        "secret: bool = false",
        "fn token(&self) -> Self",
        "private long token;",
    ] {
        assert_eq!(redact_text(s), s, "must not redact a type declaration: {s}");
    }
    let out = redact_text("password: usize4Hunter2");
    assert!(
        out.contains("[REDACTED"),
        "a real value stays masked: {out}"
    );
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

#[test]
fn key_value_redaction_never_crosses_a_line_break() {
    let samples = [
        concat!("token:", "\n", "refreshtoken: harmless"),
        concat!("Authorization: Bearer", "\n", "next_line_word"),
        concat!(
            "credential:",
            "\n  ",
            "ABCDEFGHIJKLMNOP",
            "QRSTUVWXYZabcdef",
            "012345"
        ),
    ];
    for s in samples {
        assert_eq!(redact_text(s), s, "redaction crossed a line: {s:?}");
    }
    let same_line_secret = ["token: ", "abc123", "def456", "ghi789"].concat();
    let out = redact_text(&same_line_secret);
    assert!(
        out.contains("[REDACTED:API key param]"),
        "same-line secrets must remain redacted: {out}"
    );
}

#[test]
fn asterisk_masks_survive_redaction() {
    for s in [
        "password: *****",
        "password: ***",
        r#"expected := []string{"password: *****", "title: My Home"}"#,
        "token=*",
    ] {
        assert_eq!(redact_text(s), s, "mask changed: {s}");
    }
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

/// A knowledge fact's own key is a slug, not a secret: every long one
/// was masked to the same marker, merging distinct facts on export and
/// import. Only that member, in its quoted JSON form, with words for
/// parts, is kept; every other place and shape stays masked.
#[test]
fn only_a_word_slug_as_a_fact_key_is_kept() {
    for kept in [
        r#"{"key":"governor-design-pull-forward-2026-10-29"}"#,
        r#"{"key": "hive-desktop-knowledge-speed-round-one"}"#,
        // Words with one vowel in six or seven letters, abbreviations.
        r#"{"key":"gotcha-code-reviewer-no-probe-rights"}"#,
        r#"{"key":"hive-mr-ci-scripts-strict-backlog-check"}"#,
    ] {
        assert_eq!(redact_text(kept), kept);
    }
    let masked_cases = [
        // Random letter groups, a hex digest, a long number.
        r#"{"key":"qhztvbkr-plmnwxsd-rtvbnmkl-zxcvbnml"}"#.to_owned(),
        r#"{"key":"deadbeef-cafebabe-facefeed-0123456789"}"#.to_owned(),
        format!(r#"{{"key":"my-pin-{}"}}"#, "4".repeat(26)),
        // Mostly vowel-free groups: abbreviations do not make a slug.
        r#"{"key":"zxq-vbk-pwm-rtk-qzx-bnm-lkj-word-also"}"#.to_owned(),
        // A slug under any other name, or in free text.
        r#"{"auth":"governor-design-pull-forward-2026-10-29"}"#.to_owned(),
        r#"{"credential":"governor-design-pull-forward-2026-10-29"}"#.to_owned(),
        r#"{"encryption_key":"correct-horse-battery-staple-orbit-lamp"}"#.to_owned(),
        "SIGNING_KEY=governor-design-pull-forward-2026-10-29".to_owned(),
        "MASTER_KEY=governor-design-pull-forward-2026-10-29".to_owned(),
        "key: hive-desktop-knowledge-speed-round-one".to_owned(),
    ];
    for masked in masked_cases {
        assert!(redact_text(&masked).contains("[REDACTED"), "{masked}");
    }
}

/// The fact-key exemption must not let random keys through: lowercase
/// letter groups joined by `-` under a JSON `key` member stay masked by
/// the redactor and found by secret detection (`high_entropy_secret`,
/// 40+ characters) about as often as before the exemption existed
/// (at most ~4% of a shape reads as words). Seeded, so deterministic.
#[test]
fn random_lowercase_keys_under_a_key_member_stay_masked() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut letter = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        char::from(b'a' + u8::try_from(state % 26).unwrap_or(0))
    };
    let mut random_key = |groups: usize, size: usize| {
        (0..groups)
            .map(|_| (0..size).map(|_| letter()).collect::<String>())
            .collect::<Vec<_>>()
            .join("-")
    };
    const SAMPLES: usize = 2000;
    const MAX_KEPT: usize = SAMPLES * 6 / 100;
    for (groups, size) in [(8, 4), (9, 3), (12, 2), (5, 6)] {
        let kept = (0..SAMPLES)
            .filter(|_| {
                let doc = format!(r#"{{"key":"{}"}}"#, random_key(groups, size));
                !redact_text(&doc).contains("[REDACTED")
            })
            .count();
        assert!(
            kept <= MAX_KEPT,
            "{groups}x{size}: {kept} of {SAMPLES} random keys kept"
        );
    }
    for (groups, size) in [(10, 4), (11, 3), (14, 2), (6, 6)] {
        let missed = (0..SAMPLES)
            .filter(|_| {
                let doc = format!(r#"{{"key":"{}"}}"#, random_key(groups, size));
                crate::core::secret_detection::detect_secrets(&doc).is_empty()
            })
            .count();
        assert!(
            missed <= MAX_KEPT,
            "{groups}x{size}: {missed} of {SAMPLES} random keys undetected"
        );
    }
}
