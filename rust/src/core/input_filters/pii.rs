//! PII detection with checksum guardrails (GL #675).
//!
//! Each detector pairs a regex with a *validator* so structurally-valid noise
//! (a random 16-digit order number, a phone number that looks card-shaped) is
//! not flagged. Cards use the Luhn checksum, IBANs the ISO-7064 mod-97 check,
//! and Swiss AHV/AVS numbers the EAN-13 check digit. The result is a low
//! false-positive surface suitable for redacting content before it reaches the
//! model.
//!
//! Privacy: callers receive only `(class, count)` pairs — never the matched
//! value — so audit logs can record *that* PII was present without leaking it.

use std::sync::OnceLock;

use regex::{Captures, Regex};

/// Supported PII categories. Their stable lowercase names are used in redaction
/// markers and privacy-preserving audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PiiKind {
    ChAhv,
    Iban,
    Card,
    Email,
    Phone,
    Ssn,
}

impl PiiKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ChAhv => "ch_ahv",
            Self::Iban => "iban",
            Self::Card => "card",
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Ssn => "ssn",
        }
    }
}

/// One PII detector: a labelled regex plus a checksum/shape validator. A match
/// is only counted/redacted when `validate` accepts the exact matched text.
struct PiiRule {
    kind: PiiKind,
    re: Regex,
    validate: fn(&str) -> bool,
}

fn rules() -> &'static [PiiRule] {
    static RULES: OnceLock<Vec<PiiRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            // Swiss AHV/AVS social-security number (EAN-13, prefixed 756).
            PiiRule {
                kind: PiiKind::ChAhv,
                re: Regex::new(r"\b756[.\s]?\d{4}[.\s]?\d{4}[.\s]?\d{2}\b")
                    .expect("valid AHV regex"),
                validate: ahv_valid,
            },
            // IBAN — run before the card rule so its digits aren't re-matched.
            PiiRule {
                kind: PiiKind::Iban,
                re: Regex::new(r"\b[A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]){11,30}\b")
                    .expect("valid IBAN regex"),
                validate: iban_valid,
            },
            // Payment card (13–19 digits, optional space/hyphen groups), Luhn.
            PiiRule {
                kind: PiiKind::Card,
                re: Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").expect("valid card regex"),
                validate: luhn_valid,
            },
            // Email — specific enough that no extra validation is needed.
            PiiRule {
                kind: PiiKind::Email,
                re: Regex::new(r"\b[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}\b")
                    .expect("valid email regex"),
                validate: |_| true,
            },
            // US Social Security number.
            PiiRule {
                kind: PiiKind::Ssn,
                re: Regex::new(r"\d{3}-\d{2}-\d{4}").expect("valid SSN regex"),
                validate: |_| true,
            },
            // Phone candidates: explicit E.164 or commonly formatted local
            // numbers. Shape/context validation below rejects ordinary bare
            // repository numbers such as years, issue IDs, ports, and counts.
            // This runs after structured identifiers to avoid overlapping matches.
            PiiRule {
                kind: PiiKind::Phone,
                re: Regex::new(
                    r"(?:\+?[1-9]\d{0,2}[ .-])?(?:\(?\d{2,4}\)?[ .-]){1,3}\d{2,4}|\+?[1-9]\d{1,14}",
                )
                .expect("valid phone regex"),
                validate: |_| true,
            },
        ]
    })
}

/// Redact every validated PII match to `[REDACTED:<class>]`. Returns the
/// transformed text plus per-class hit counts (for privacy-preserving audit).
#[must_use]
pub fn redact(text: &str) -> (String, Vec<(&'static str, usize)>) {
    let mut out = text.to_string();
    let mut counts = Vec::new();
    for rule in rules() {
        let mut n = 0usize;
        let source = out.clone();
        out = rule
            .re
            .replace_all(&source, |caps: &Captures| {
                let m = caps.get(0).map_or("", |g| g.as_str());
                let valid_phone_shape = rule.kind != PiiKind::Phone
                    || caps
                        .get(0)
                        .is_some_and(|matched| phone_match_is_valid(&source, matched));
                if (rule.validate)(m) && valid_phone_shape {
                    n += 1;
                    format!("[REDACTED:{}]", rule.kind.as_str())
                } else {
                    m.to_string()
                }
            })
            .to_string();
        if n > 0 {
            counts.push((rule.kind.as_str(), n));
        }
    }
    (out, counts)
}

/// Accept only phone-shaped candidates, not arbitrary digit runs. Explicit
/// E.164 numbers are sufficient by themselves. Other candidates need familiar
/// formatting or a nearby phone label, and every candidate must carry enough
/// digits to be a plausible subscriber number.
fn phone_match_is_valid(text: &str, matched: regex::Match<'_>) -> bool {
    let before = text[..matched.start()].chars().next_back();
    let after = text[matched.end()..].chars().next();
    let delimited = [before, after]
        .into_iter()
        .flatten()
        .all(|character| !character.is_alphanumeric() && !matches!(character, '.' | '-'));
    if !delimited {
        return false;
    }
    if overlaps_checksum_identifier(text, &matched) {
        return false;
    }

    let candidate = matched.as_str();
    let digit_count = candidate.bytes().filter(u8::is_ascii_digit).count();
    if !(7..=15).contains(&digit_count) {
        return false;
    }
    if candidate.starts_with('+') {
        return true;
    }
    if phone_label_near(text, &matched) {
        return true;
    }

    // Without an explicit prefix or label, keep the accepted shape narrow.
    // This avoids reclassifying failed checksum identifiers or date/build
    // sequences while covering the common formats reported in #1682.
    let groups = candidate
        .split(|character: char| !character.is_ascii_digit())
        .filter(|group| !group.is_empty())
        .map(str::len)
        .collect::<Vec<_>>();
    matches!(groups.as_slice(), [3, 3, 4] | [3 | 2, 3, 2, 2] | [2, 4, 4])
}

fn overlaps_checksum_identifier(text: &str, phone: &regex::Match<'_>) -> bool {
    rules()
        .iter()
        .filter(|rule| matches!(rule.kind, PiiKind::ChAhv | PiiKind::Iban | PiiKind::Card))
        .any(|rule| {
            rule.re.find_iter(text).any(|identifier| {
                phone.start() < identifier.end() && identifier.start() < phone.end()
            })
        })
}

fn phone_label_near(text: &str, matched: &regex::Match<'_>) -> bool {
    const LABELS: &[&str] = &["call", "fax", "mobile", "phone", "tel", "telephone"];
    const CONTEXT_CHARS: usize = 24;
    let before_context = text[..matched.start()]
        .chars()
        .rev()
        .take(CONTEXT_CHARS)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let after_context = text[matched.end()..]
        .chars()
        .take(CONTEXT_CHARS)
        .collect::<String>();
    let before = before_context
        .rsplit(|character: char| !character.is_ascii_alphabetic())
        .find(|part| !part.is_empty())
        .unwrap_or("");
    let after = after_context
        .split(|character: char| !character.is_ascii_alphabetic())
        .find(|part| !part.is_empty())
        .unwrap_or("");
    LABELS
        .iter()
        .any(|label| before.eq_ignore_ascii_case(label) || after.eq_ignore_ascii_case(label))
}

/// Count validated PII matches per class without rewriting (for block
/// decisions). Empty = no PII detected.
#[must_use]
pub fn detect(text: &str) -> Vec<(&'static str, usize)> {
    // Apply the same priority as redaction so a card, AHV, or SSN is not also
    // reported as a phone number.
    redact(text).1
}

fn digits(s: &str) -> Vec<u32> {
    s.chars().filter_map(|c| c.to_digit(10)).collect()
}

/// Luhn checksum for payment cards (13–19 digits).
fn luhn_valid(s: &str) -> bool {
    let d = digits(s);
    if d.len() < 13 || d.len() > 19 {
        return false;
    }
    let mut sum = 0u32;
    let mut double = false;
    for &digit in d.iter().rev() {
        let mut x = digit;
        if double {
            x *= 2;
            if x > 9 {
                x -= 9;
            }
        }
        sum += x;
        double = !double;
    }
    sum.is_multiple_of(10)
}

/// EAN-13 check digit for a Swiss AHV number (must start 756, 13 digits).
fn ahv_valid(s: &str) -> bool {
    let d = digits(s);
    if d.len() != 13 || d[0] != 7 || d[1] != 5 || d[2] != 6 {
        return false;
    }
    let mut sum = 0u32;
    for (i, &digit) in d[..12].iter().enumerate() {
        sum += if i.is_multiple_of(2) {
            digit
        } else {
            digit * 3
        };
    }
    let check = (10 - (sum % 10)) % 10;
    check == d[12]
}

/// ISO-7064 mod-97 check for an IBAN.
fn iban_valid(s: &str) -> bool {
    let compact: String = s
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    if compact.len() < 15 || compact.len() > 34 {
        return false;
    }
    let (head, tail) = compact.split_at(4);
    let mut remainder = 0u32;
    for c in tail.chars().chain(head.chars()) {
        if let Some(dval) = c.to_digit(10) {
            remainder = (remainder * 10 + dval) % 97;
        } else {
            // Letter → two-digit value (A=10 … Z=35).
            let v = (c as u32) - ('A' as u32) + 10;
            remainder = (remainder * 100 + v) % 97;
        }
    }
    remainder == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_valid_ch_ahv() {
        // 756.9217.0769.85 is a valid EAN-13 AHV test number.
        let (out, counts) = redact("AHV 756.9217.0769.85 on file");
        assert!(out.contains("[REDACTED:ch_ahv]"), "{out}");
        assert_eq!(counts, vec![("ch_ahv", 1)]);
    }

    #[test]
    fn ignores_ahv_with_bad_checksum() {
        let (out, counts) = redact("756.9217.0769.86 is not valid");
        assert!(
            out.contains("756.9217.0769.86"),
            "must not redact invalid AHV"
        );
        assert!(counts.is_empty());
    }

    #[test]
    fn redacts_valid_card_via_luhn() {
        // 4111 1111 1111 1111 is the canonical Luhn-valid Visa test number.
        let (out, _) = redact("card 4111 1111 1111 1111 expires");
        assert!(out.contains("[REDACTED:card]"), "{out}");
    }

    #[test]
    fn ignores_non_luhn_16_digits() {
        // A random 16-digit order id that fails Luhn must survive.
        let (out, counts) = redact("order 1234567890123456 shipped");
        assert!(out.contains("1234567890123456"), "false positive: {out}");
        assert!(counts.is_empty());
    }

    #[test]
    fn redacts_valid_iban() {
        // GB82 WEST 1234 5698 7654 32 is a valid mod-97 IBAN.
        let (out, _) = redact("pay to GB82 WEST 1234 5698 7654 32 today");
        assert!(out.contains("[REDACTED:iban]"), "{out}");
    }

    #[test]
    fn ignores_invalid_iban() {
        let (out, counts) = redact("ref GB00WEST12345698765432 here");
        assert!(out.contains("GB00WEST12345698765432"));
        assert!(counts.is_empty());
    }

    #[test]
    fn redacts_email() {
        let (out, _) = redact("contact jane.doe@example.com please");
        assert!(out.contains("[REDACTED:email]"), "{out}");
    }

    #[test]
    fn redacts_e164_phone() {
        let (out, counts) = redact("call +14155552671");
        assert_eq!(out, "call [REDACTED:phone]");
        assert_eq!(counts, vec![("phone", 1)]);
    }

    #[test]
    fn phone_detection_requires_phone_shape_or_context() {
        for text in [
            "aa 25 bb",
            "aa 2026 bb",
            "fixes #1234",
            "run 17384920156",
            "port 8069",
            "at 14:22:05 UTC",
            "pattern {0,120} bound",
            "lean-ctx 3.10.0",
            "aa 2026-08-31 bb",
            "build 2026-08-31 1234",
            "invalid AHV 756 9217 0769 86",
            "invalid card 1234-5678-9012-3",
            "invalid card 123 456 7890 1234",
            "invalid IBAN CH00 0076 2011 6238 5295 7",
            "unicode-adjacent é612-338-6000",
        ] {
            assert!(detect(text).is_empty(), "false positive: {text}");
        }

        for text in [
            "call +14155552671",
            "call 6123386000",
            "phone: 6123386000",
            "aa 612-338-6000 bb",
            "aa 612.338.6000 bb",
            "aa +1 612-338-6000 bb",
            "aa (612) 338-6000 bb",
        ] {
            assert!(
                detect(text).iter().any(|(class, _)| *class == "phone"),
                "missed phone: {text}"
            );
        }
    }

    #[test]
    fn redacts_us_ssn() {
        let (out, counts) = redact("SSN 123-45-6789");
        assert_eq!(out, "SSN [REDACTED:ssn]");
        assert_eq!(counts, vec![("ssn", 1)]);
    }

    #[test]
    fn detect_counts_match_redact() {
        let text = "jane@example.com, +14155552671, 123-45-6789, and 4111 1111 1111 1111";
        let counts = detect(text);
        assert!(counts.iter().any(|(c, _)| *c == "email"));
        assert!(counts.iter().any(|(c, _)| *c == "phone"));
        assert!(counts.iter().any(|(c, _)| *c == "ssn"));
        assert!(counts.iter().any(|(c, _)| *c == "card"));
    }

    #[test]
    fn clean_text_has_no_pii() {
        assert!(detect("just some ordinary source code, no secrets").is_empty());
    }
}
