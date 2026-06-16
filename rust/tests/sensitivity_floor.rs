//! Integration tests for the per-item sensitivity model + policy floor (#212).
//!
//! Exercises the public `core::sensitivity` API end-to-end: classification of
//! real secret/PII vectors, the floor × action matrix, and level ordering.
//! All deterministic — no env, no config files, no global state.

use lean_ctx::core::sensitivity::{
    Enforced, FloorAction, SensitivityConfig, SensitivityLevel, classify, classify_content,
    enforce_text,
};
use std::path::Path;

fn cfg(enabled: bool, floor: SensitivityLevel, action: FloorAction) -> SensitivityConfig {
    SensitivityConfig {
        enabled,
        policy_floor: floor,
        action,
    }
}

#[test]
fn disabled_is_a_full_noop() {
    let cfg = SensitivityConfig::default(); // enabled == false
    let secret = "aws AKIAIOSFODNN7EXAMPLE here".to_string();
    assert_eq!(
        enforce_text(secret.clone(), None, &cfg),
        Enforced::Pass(secret)
    );
}

#[test]
fn secret_redacted_keeps_surrounding_text() {
    let c = cfg(true, SensitivityLevel::Secret, FloorAction::Redact);
    match enforce_text("before AKIAIOSFODNN7EXAMPLE after".to_string(), None, &c) {
        Enforced::Redacted { text, level } => {
            assert_eq!(level, SensitivityLevel::Secret);
            assert!(text.contains("before") && text.contains("after"));
            assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"));
        }
        other => panic!("expected Redacted, got {other:?}"),
    }
}

#[test]
fn secret_dropped_when_action_is_drop() {
    let c = cfg(true, SensitivityLevel::Secret, FloorAction::Drop);
    match enforce_text(
        "token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".to_string(),
        None,
        &c,
    ) {
        Enforced::Dropped { level, notice } => {
            assert_eq!(level, SensitivityLevel::Secret);
            assert!(notice.contains("withheld"));
        }
        other => panic!("expected Dropped, got {other:?}"),
    }
}

#[test]
fn confidential_pii_blocked_at_confidential_floor() {
    let c = cfg(true, SensitivityLevel::Confidential, FloorAction::Redact);
    // 4111 1111 1111 1111 is the canonical Luhn-valid Visa test number.
    match enforce_text(
        "charge card 4111 1111 1111 1111 today".to_string(),
        None,
        &c,
    ) {
        Enforced::Redacted { text, level } => {
            assert_eq!(level, SensitivityLevel::Confidential);
            assert!(text.contains("[REDACTED:card]"));
            assert!(text.contains("charge") && text.contains("today"));
        }
        other => panic!("expected Redacted, got {other:?}"),
    }
}

#[test]
fn confidential_passes_when_floor_is_secret() {
    // A card number is Confidential, which is BELOW a Secret floor → untouched.
    let c = cfg(true, SensitivityLevel::Secret, FloorAction::Drop);
    let text = "card 4111 1111 1111 1111".to_string();
    assert_eq!(enforce_text(text.clone(), None, &c), Enforced::Pass(text));
}

#[test]
fn benign_text_always_passes_when_enabled() {
    let c = cfg(true, SensitivityLevel::Confidential, FloorAction::Drop);
    let text = "deployment finished in 12 seconds, 0 errors".to_string();
    assert_eq!(enforce_text(text.clone(), None, &c), Enforced::Pass(text));
}

#[test]
fn path_hint_raises_level_even_for_benign_content() {
    let c = cfg(true, SensitivityLevel::Secret, FloorAction::Drop);
    // Benign body but a secret-like path → Secret → dropped.
    let out = enforce_text(
        "PORT=8080\n".to_string(),
        Some(Path::new("/srv/app/.env")),
        &c,
    );
    assert!(matches!(out, Enforced::Dropped { .. }));
}

#[test]
fn classification_vectors_are_precise() {
    assert_eq!(
        classify_content("nothing sensitive here, just text"),
        SensitivityLevel::Public
    );
    // Non-Luhn 16-digit number must NOT be flagged.
    assert_eq!(
        classify_content("invoice 1234567890123456"),
        SensitivityLevel::Public
    );
    // Valid IBAN (mod-97) → Confidential.
    assert_eq!(
        classify_content("iban DE89370400440532013000"),
        SensitivityLevel::Confidential
    );
    // Secret beats path-public.
    assert_eq!(
        classify(
            Some(Path::new("src/main.rs")),
            "key = ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        ),
        SensitivityLevel::Secret
    );
}
