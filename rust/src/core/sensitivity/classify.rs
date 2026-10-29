//! Classification + redaction helpers for the sensitivity model.
//!
//! Only high-precision signals raise a level, to avoid false positives:
//! - secret-like paths ([`is_secret_like`]) and detected secrets → `Secret`
//! - Luhn-validated card numbers → `Confidential`

use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use super::SensitivityLevel;
use crate::core::{io_boundary, secret_detection};

/// Classify a source path. Secret-like paths (keys, `.env`, `.ssh/…`) → `Secret`.
/// Everything else stays `Public` — path alone is not enough to infer lower
/// confidential levels without guessing.
pub fn classify_path(path: &Path) -> SensitivityLevel {
    if io_boundary::is_secret_like(path).is_some() {
        SensitivityLevel::Secret
    } else {
        SensitivityLevel::Public
    }
}

/// Classify free text by content.
pub fn classify_content(content: &str) -> SensitivityLevel {
    if !secret_detection::detect_secrets(content).is_empty() {
        return SensitivityLevel::Secret;
    }
    if has_credit_card(content) {
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
/// scanner) plus card numbers, which the secret scanner does not cover.
pub(super) fn redact_sensitive(text: &str) -> String {
    // Force redaction on regardless of the global `secret_detection` toggle: a
    // sensitivity floor must always mask, not merely detect.
    let forced = crate::core::config::SecretDetectionConfig {
        enabled: true,
        redact: true,
        ..Default::default()
    };
    let (secrets_masked, _) = secret_detection::scan_and_redact(text, &forced);
    redact_credit_cards(&secrets_masked)
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
        let mut v = d as u32;
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

/// A card-shaped (see `card_layout_plausible`), Luhn-valid candidate that is
/// not a segment of a version token (#2135).
fn is_card(text: &str, candidate: &regex::Match<'_>) -> bool {
    crate::core::input_filters::pii::card_layout_plausible(candidate.as_str())
        && luhn_valid(&digits_of(candidate.as_str()))
        && !crate::core::input_filters::pii::inside_version_token(text, candidate)
}

fn has_credit_card(content: &str) -> bool {
    card_re().find_iter(content).any(|m| is_card(content, &m))
}

fn redact_credit_cards(text: &str) -> String {
    card_re()
        .replace_all(text, |caps: &regex::Captures| {
            let m = caps.get(0).map(|x| x.as_str()).unwrap_or_default();
            if caps.get(0).is_some_and(|matched| is_card(text, &matched)) {
                "[REDACTED:card]".to_string()
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
        let t = concat!("export TOKEN=gh", "p_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789");
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
    fn redact_masks_card_and_leaves_an_iban_to_filter_packs() {
        let red = redact_sensitive("card 4111 1111 1111 1111 iban DE89370400440532013000 end");
        assert!(red.contains("[REDACTED:card]"));
        // IBANs are a Hive filter pack, not a built-in detector.
        assert!(red.contains("DE89370400440532013000"));
        assert!(red.contains("end"));
        assert!(!red.contains("4111 1111 1111 1111"));
    }

    #[test]
    fn timestamped_names_are_not_cards_even_when_luhn_valid() {
        let stamp = (0..10)
            .map(|d| format!("20261006-16153{d}"))
            .find(|s| luhn_valid(&digits_of(s)))
            .expect("one check digit makes the stamp Luhn-valid");
        let text = format!("session {stamp} archived");
        assert!(!has_credit_card(&text));
        assert_eq!(redact_credit_cards(&text), text);
    }

    #[test]
    fn go_pseudo_version_stamps_are_not_cards() {
        // #2135: the stamp segment of a Go pseudo-version.
        let stamp = (0..10)
            .map(|d| format!("2026090112454{d}"))
            .find(|s| luhn_valid(&digits_of(s)))
            .expect("one check digit makes the stamp Luhn-valid");
        let text = format!("github.com/c/d v0.6.1-0.{stamp}-3de7acda5f88");
        assert!(!has_credit_card(&text));
        assert_eq!(redact_credit_cards(&text), text);
        let card = format!(
            "card {} on file",
            ["4111", "1111", "1111", "1111"].join(" ")
        );
        assert!(has_credit_card(&card));
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
