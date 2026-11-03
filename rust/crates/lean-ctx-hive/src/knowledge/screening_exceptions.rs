// SPDX-License-Identifier: Apache-2.0
//! Narrow screening exceptions (owner decision 03.11.2026), identical in the
//! LeanCTX instance and the Hive server (`hive-screen/src/exceptions.rs`). C0
//! `vectors/screening_exceptions.vectors.json` is their single source; a test
//! below asserts every case.
//!
//! [`neutralise`] replaces exactly four shapes with neutral words before the
//! vendored scanners run, so those files stay byte for byte the engine's:
//!
//! - an `Authorization: Bearer|Basic` header whose value is only a
//!   documentation placeholder (`…`, `...`, `<name>`, `[REDACTED…]`, a run of
//!   3+ `x`/`X`, `$NAME`, `${…}`, `{name}`, or a lowercase token-type prefix
//!   elided with `…`/`...`, e.g. `hive_p_…`);
//! - one of Stripe's documented test card numbers (digits compared exactly,
//!   separators ignored, no prefix rule);
//! - a whole token `ed25519-<32 lowercase hex>`, `sha256:`/`blake3:`/`sha256-`
//!   followed by 64 lowercase hex (bounded by no secret-value character);
//! - a redaction marker `[REDACTED…]` inside a one-line backtick code span,
//!   where docs show the marker's format (it holds no secret either way).

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// Stripe's documented test card numbers (stripe.com/docs/testing, "Card
/// numbers" by brand), digits only. Assembled from groups so the source
/// never carries a contiguous card number.
pub const STRIPE_TEST_CARDS: [&str; 17] = [
    concat!("4242", "4242", "4242", "4242"),        // Visa
    concat!("4000", "0566", "5566", "5556"),        // Visa (debit)
    concat!("5555", "5555", "5555", "4444"),        // Mastercard
    concat!("2223", "0031", "2200", "3222"),        // Mastercard (2-series)
    concat!("5200", "8282", "8282", "8210"),        // Mastercard (debit)
    concat!("5105", "1051", "0510", "5100"),        // Mastercard (prepaid)
    concat!("3782", "8224", "6310", "005"),         // American Express
    concat!("3714", "4963", "5398", "431"),         // American Express
    concat!("6011", "1111", "1111", "1117"),        // Discover
    concat!("6011", "0009", "9013", "9424"),        // Discover
    concat!("6011", "9811", "1111", "1113"),        // Discover (debit)
    concat!("3056", "9300", "0902", "0004"),        // Diners Club
    concat!("3622", "7206", "2716", "67"),          // Diners Club (14-digit)
    concat!("3566", "0020", "2036", "0505"),        // JCB
    concat!("6200", "0000", "0000", "0005"),        // UnionPay
    concat!("6200", "0000", "0000", "0047"),        // UnionPay (debit)
    concat!("6205", "5000", "0000", "0000", "004"), // UnionPay (19-digit)
];

/// Whether `candidate` (any separators) is one of [`STRIPE_TEST_CARDS`].
pub fn is_stripe_test_card(candidate: &str) -> bool {
    let digits: String = candidate.chars().filter(char::is_ascii_digit).collect();
    STRIPE_TEST_CARDS.contains(&digits.as_str())
}

fn regex(slot: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    slot.get_or_init(|| Regex::new(pattern).expect("static exception pattern"))
}

fn authorization_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    regex(&RE, r"(?i)authorization[ \t]*:[ \t]*(?:bearer|basic)[ \t]+")
}

/// The card candidate of `pii.rs`.
fn card_candidate_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    regex(&RE, r"(?-u:\b)\d(?:[ -]?\d){12,18}(?-u:\b)")
}

fn digest_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    regex(
        &RE,
        r"ed25519-[0-9a-f]{32}|(?:sha256|blake3):[0-9a-f]{64}|sha256-[0-9a-f]{64}",
    )
}

/// A one-line backtick code span.
fn code_span_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    regex(&RE, r"`[^`\n]+`")
}

/// A redaction marker up to its `]` on the same line.
fn marker_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    regex(&RE, r"\[REDACTED[^\]\n]*\]")
}

/// The length of the placeholder at the start of `rest` (the text after
/// `Authorization: Bearer `), or `None` for a real-looking value.
fn placeholder_len(rest: &str) -> Option<usize> {
    if rest.starts_with("[REDACTED") {
        let line = rest.split('\n').next().unwrap_or("");
        return line.find(']').map(|i| i + 1);
    }
    let token = rest
        .split(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ',' | ';' | ')'))
        .next()
        .unwrap_or("");
    is_placeholder_token(token).then_some(token.len())
}

fn is_placeholder_token(token: &str) -> bool {
    if matches!(token, "…" | "...") {
        return true;
    }
    if token.len() >= 3 && token.bytes().all(|b| matches!(b, b'x' | b'X')) {
        return true;
    }
    let wrapped = |open: &str, close: char, forbidden: &[char]| {
        token
            .strip_prefix(open)
            .and_then(|t| t.strip_suffix(close))
            .is_some_and(|inner| !inner.is_empty() && !inner.contains(forbidden))
    };
    if wrapped("<", '>', &['<', '>']) || wrapped("${", '}', &['{', '}']) {
        return true;
    }
    if wrapped("{", '}', &['{', '}']) {
        return true;
    }
    if let Some(name) = token.strip_prefix('$') {
        let mut chars = name.chars();
        return chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    }
    // A token-type prefix without its body: `hive_p_…`, `hive_scim_...`.
    // `sk_live_51H…` (digits, uppercase) is no such prefix and still hits.
    if let Some(prefix) = token
        .strip_suffix('…')
        .or_else(|| token.strip_suffix("..."))
    {
        return prefix.ends_with('_')
            && prefix.starts_with(|c: char| c.is_ascii_lowercase())
            && prefix.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    }
    false
}

/// A character a key/value secret value can contain: a digest bounded by
/// one is not the whole token.
fn is_value_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')
}

/// `text` with every excepted span replaced by a neutral word; borrowed when
/// there is none.
pub fn neutralise(text: &str) -> Cow<'_, str> {
    let mut spans: Vec<(usize, usize, &str)> = Vec::new();
    for m in authorization_prefix_re().find_iter(text) {
        if let Some(len) = placeholder_len(&text[m.end()..]) {
            spans.push((m.start(), m.end() + len, "auth-placeholder"));
        }
    }
    for m in card_candidate_re().find_iter(text) {
        if is_stripe_test_card(m.as_str()) {
            spans.push((m.start(), m.end(), "test-card"));
        }
    }
    for m in digest_re().find_iter(text) {
        let before = text[..m.start()].chars().next_back();
        let after = text[m.end()..].chars().next();
        if !before.is_some_and(is_value_char) && !after.is_some_and(is_value_char) {
            spans.push((m.start(), m.end(), "digest"));
        }
    }
    for span in code_span_re().find_iter(text) {
        for m in marker_re().find_iter(span.as_str()) {
            spans.push((
                span.start() + m.start(),
                span.start() + m.end(),
                "redaction-marker",
            ));
        }
    }
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, word) in spans {
        if start < at {
            continue;
        }
        out.push_str(&text[at..start]);
        out.push_str(word);
        at = end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luhn_valid(digits: &str) -> bool {
        let mut sum = 0u32;
        for (i, d) in digits.bytes().rev().enumerate() {
            let mut x = u32::from(d - b'0');
            if i % 2 == 1 {
                x *= 2;
                if x > 9 {
                    x -= 9;
                }
            }
            sum += x;
        }
        sum.is_multiple_of(10)
    }

    #[test]
    fn stripe_test_cards_are_real_luhn_valid_card_numbers() {
        for card in STRIPE_TEST_CARDS {
            assert!(card.bytes().all(|b| b.is_ascii_digit()), "{}", card.len());
            assert!((14..=19).contains(&card.len()), "{}", card.len());
            assert!(luhn_valid(card), "a Stripe test card fails Luhn");
            assert!(is_stripe_test_card(card));
        }
    }

    /// One case of the C0 vector file (one compact object per line).
    struct Case {
        input: String,
        detector: String,
        hit: bool,
        /// Dogfood lines: the whole screen must pass them.
        source: Option<String>,
    }

    fn unescape(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let code: String = chars.by_ref().take(4).collect();
                    let n = u32::from_str_radix(&code, 16).expect("\\u escape");
                    out.push(char::from_u32(n).expect("BMP scalar"));
                }
                Some(other) => out.push(other),
                None => panic!("dangling escape"),
            }
        }
        out
    }

    fn cases() -> Vec<Case> {
        let text = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../lean-ctx-protocol/tests/fixtures/hive-v2/vectors/screening_exceptions.vectors.json"
        ));
        let re = Regex::new(
            r#"\{"input": "((?:[^"\\]|\\.)*)", "detector": "([a-z_]+)", "hit": (true|false)(?:, "source": "([^"]+)")?\}"#,
        )
        .unwrap();
        let cases: Vec<Case> = re
            .captures_iter(text)
            .map(|c| Case {
                input: unescape(&c[1]),
                detector: c[2].to_string(),
                hit: &c[3] == "true",
                source: c.get(4).map(|s| s.as_str().to_string()),
            })
            .collect();
        assert_eq!(
            cases.len(),
            text.matches("{\"input\":").count(),
            "every case parses"
        );
        assert!(cases.len() > 30);
        cases
    }

    /// Whether `detector` (or its redaction twin) fires on `input` after the
    /// exceptions.
    fn fires(detector: &str, input: &str) -> bool {
        let text = neutralise(input);
        let secrets = lean_ctx::core::secret_detection::detect_secrets(&text);
        let named = |names: &[&str]| secrets.iter().any(|m| names.contains(&m.pattern_name));
        let redacted = lean_ctx::core::redaction::redact_text(&text);
        let masked = |labels: &[&str]| {
            labels
                .iter()
                .any(|l| redacted.contains(&format!("[REDACTED:{l}]")))
        };
        match detector {
            "authorization_header" => {
                named(&["authorization_header"]) || masked(&["Authorization header"])
            }
            "card" => lean_ctx::core::input_filters::pii::detect(&text)
                .iter()
                .any(|(k, _)| *k == "card"),
            "high_entropy_secret" => {
                named(&[
                    "generic_api_key",
                    "high_entropy_secret",
                    "url_encoded_secret",
                ]) || masked(&["Generic long secret", "API key param", "URL-encoded secret"])
            }
            "redaction_marker" => text.contains("[REDACTED"),
            other => panic!("unknown detector {other}"),
        }
    }

    #[test]
    fn screening_exception_vectors() {
        let cases = cases();
        // Indices only in messages: the inputs carry card numbers.
        let mut failures = Vec::new();
        for (i, case) in cases.iter().enumerate() {
            if fires(&case.detector, &case.input) != case.hit {
                failures.push(format!("case {i} ({}): hit != {}", case.detector, case.hit));
            }
            if let Some(source) = &case.source
                && crate::knowledge::screening::Screen::with_rules(
                    Vec::new(),
                    std::sync::Arc::new(Vec::new()),
                    Vec::new(),
                )
                .check(&case.input)
                .is_err()
            {
                failures.push(format!("case {i} ({source}): the screen refuses it"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
        for card in STRIPE_TEST_CARDS {
            assert!(
                cases.iter().any(|c| c.detector == "card"
                    && !c.hit
                    && card_candidate_re().find_iter(&c.input).any(|m| {
                        m.as_str()
                            .chars()
                            .filter(char::is_ascii_digit)
                            .collect::<String>()
                            == card
                    })),
                "a Stripe card ({} digits) has no vector",
                card.len()
            );
        }
        assert!(cases.iter().filter(|c| c.source.is_some()).count() >= 7);
    }

    #[test]
    fn without_exceptions_the_dogfood_lines_were_refused() {
        let refused = cases()
            .iter()
            .filter(|c| c.source.is_some())
            .filter(|c| {
                lean_ctx::core::secret_detection::detect_secrets(&c.input).len()
                    + usize::from(lean_ctx::core::redaction::redact_text(&c.input) != c.input)
                    + lean_ctx::core::input_filters::pii::detect(&c.input).len()
                    > 0
            })
            .count();
        assert!(refused >= 4, "{refused}");
    }
}
