//! Classification + redaction helpers for the sensitivity model.
//!
//! Only high-precision signals raise a level, to avoid false positives:
//! - secret-like paths ([`is_secret_like`]) and detected secrets → `Secret`
//! - Luhn-validated card numbers and mod-97-validated IBANs → `Confidential`

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::SensitivityLevel;
use crate::core::{io_boundary, secret_detection};

/// Classify a source path. Secret-like paths (keys, `.env`, `.ssh/…`) → `Secret`.
/// Everything else stays `Public` — path alone is not enough to infer lower
/// confidential levels without guessing.
#[must_use]
pub fn classify_path(path: &Path) -> SensitivityLevel {
    if io_boundary::is_secret_like(path).is_some() {
        SensitivityLevel::Secret
    } else {
        SensitivityLevel::Public
    }
}

/// Classify free text by content.
#[must_use]
pub fn classify_content(content: &str) -> SensitivityLevel {
    if !secret_detection::detect_secrets(content).is_empty() {
        return SensitivityLevel::Secret;
    }
    if has_credit_card(content) || has_iban(content) {
        return SensitivityLevel::Confidential;
    }
    SensitivityLevel::Public
}

/// Combined classification: the maximum of path- and content-derived levels.
pub fn classify(path: Option<&Path>, content: &str) -> SensitivityLevel {
    let from_path = path.map(classify_path).unwrap_or_default();
    from_path.max(classify_content(content))
}

/// Redact the spans that raise sensitivity: known secrets (via the config-driven
/// scanner) plus PII (card numbers, IBANs) the secret scanner does not cover.
pub(super) fn redact_sensitive(text: &str) -> String {
    // Force redaction on regardless of the global `secret_detection` toggle: a
    // sensitivity floor must always mask, not merely detect.
    let forced = crate::core::config::SecretDetectionConfig {
        enabled: true,
        redact: true,
        ..Default::default()
    };
    let (secrets_masked, _) = secret_detection::scan_and_redact(text, &forced);
    let cards_masked = redact_credit_cards(&secrets_masked);
    redact_ibans(&cards_masked)
}

// ---- Card numbers (Luhn) ---------------------------------------------------

fn card_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").expect("valid card regex"))
}

/// Luhn checksum over decimal digits. Length 13–19 (standard PAN range).
fn luhn_valid(digits: &[u8]) -> bool {
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let mut sum = 0u32;
    let mut double = false;
    for &d in digits.iter().rev() {
        if d > 9 {
            return false;
        }
        let mut v = u32::from(d);
        if double {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
        double = !double;
    }
    sum.is_multiple_of(10)
}

fn digits_of(s: &str) -> Vec<u8> {
    s.chars()
        .filter(char::is_ascii_digit)
        .map(|c| c as u8 - b'0')
        .collect()
}

fn has_credit_card(content: &str) -> bool {
    card_re()
        .find_iter(content)
        .any(|m| luhn_valid(&digits_of(m.as_str())))
}

fn redact_credit_cards(text: &str) -> String {
    card_re()
        .replace_all(text, |caps: &regex::Captures| {
            let m = caps.get(0).map(|x| x.as_str()).unwrap_or_default();
            if luhn_valid(&digits_of(m)) {
                "[REDACTED:card]".to_string()
            } else {
                m.to_string()
            }
        })
        .into_owned()
}

// ---- IBANs (ISO 7064 mod-97) ----------------------------------------------

fn iban_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b[A-Za-z]{2}\d{2}[A-Za-z0-9]{11,30}\b").expect("valid iban regex")
    })
}

/// Validate an IBAN candidate via the ISO 7064 mod-97 checksum.
fn iban_valid(candidate: &str) -> bool {
    let s: String = candidate.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() < 15 || s.len() > 34 {
        return false;
    }
    if !s.is_char_boundary(4) {
        return false;
    }
    // Move the first four characters to the end, then map letters A..Z -> 10..35.
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        if let Some(d) = c.to_digit(10) {
            rem = (rem * 10 + d) % 97;
        } else if c.is_ascii_alphabetic() {
            let val = u32::from(c.to_ascii_uppercase() as u8 - b'A' + 10);
            rem = (rem * 100 + val) % 97;
        } else {
            return false;
        }
    }
    rem == 1
}

fn has_iban(content: &str) -> bool {
    iban_re().find_iter(content).any(|m| iban_valid(m.as_str()))
}

fn redact_ibans(text: &str) -> String {
    iban_re()
        .replace_all(text, |caps: &regex::Captures| {
            let m = caps.get(0).map(|x| x.as_str()).unwrap_or_default();
            if iban_valid(m) {
                "[REDACTED:iban]".to_string()
            } else {
                m.to_string()
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_content_is_secret() {
        // GitHub token shape is detected by secret_detection.
        let t = "export TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        assert_eq!(classify_content(t), SensitivityLevel::Secret);
    }

    #[test]
    fn benign_content_is_public() {
        assert_eq!(
            classify_content("the quick brown fox jumps over 12 lazy dogs"),
            SensitivityLevel::Public
        );
    }

    #[test]
    fn luhn_valid_card_is_confidential() {
        // 4111 1111 1111 1111 is the canonical Visa test number (Luhn-valid).
        assert_eq!(
            classify_content("card: 4111 1111 1111 1111 on file"),
            SensitivityLevel::Confidential
        );
    }

    #[test]
    fn random_16_digits_not_flagged_unless_luhn() {
        // 1234567890123456 is NOT Luhn-valid → must stay public.
        assert_eq!(
            classify_content("order id 1234567890123456"),
            SensitivityLevel::Public
        );
    }

    #[test]
    fn valid_iban_is_confidential() {
        // DE89 3704 0044 0532 0130 00 is a well-known valid test IBAN.
        assert_eq!(
            classify_content("pay to DE89370400440532013000 now"),
            SensitivityLevel::Confidential
        );
    }

    #[test]
    fn invalid_iban_not_flagged() {
        assert_eq!(
            classify_content("ref DE00370400440532013000"),
            SensitivityLevel::Public
        );
    }

    #[test]
    fn redact_masks_card_and_iban_keeps_text() {
        let red = redact_sensitive("card 4111 1111 1111 1111 iban DE89370400440532013000 end");
        assert!(red.contains("[REDACTED:card]"));
        assert!(red.contains("[REDACTED:iban]"));
        assert!(red.contains("end"));
        assert!(!red.contains("4111 1111 1111 1111"));
    }

    #[test]
    fn secret_like_path_is_secret() {
        assert_eq!(
            classify_path(Path::new("/home/u/.ssh/id_rsa")),
            SensitivityLevel::Secret
        );
        assert_eq!(
            classify_path(Path::new("src/main.rs")),
            SensitivityLevel::Public
        );
    }

    #[test]
    fn classify_takes_max_of_path_and_content() {
        // Benign content but secret path → Secret.
        assert_eq!(
            classify(Some(Path::new("/x/.env")), "PORT=8080"),
            SensitivityLevel::Secret
        );
    }
}
