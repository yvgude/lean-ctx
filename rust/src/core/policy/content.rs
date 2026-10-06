// SPDX-License-Identifier: Apache-2.0
//! Shared, local policy decision before content is released or persisted.
//!
//! Callers retain access/audit and format ownership. In particular, a signed
//! snapshot must reject a rewrite instead of publishing a stale content digest.

use super::runtime::ActivePolicy;
use crate::core::input_filters::{self, FilterOutcome};

/// A protected output must be inspectable as one bounded unit. Larger inputs
/// are withheld rather than partially scanned and returned as if safe.
pub const MAX_PROTECTED_CONTENT_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_REDACTION_RULES: usize = 128;
pub(super) const MAX_RULE_LABEL_BYTES: usize = 64;
pub(super) const MAX_RULE_PATTERN_BYTES: usize = 4096;

fn withheld(reason: &str) -> FilterOutcome {
    FilterOutcome {
        blocked: true,
        block_reason: Some(reason.into()),
        audit: vec![(reason.into(), 1)],
        ..FilterOutcome::default()
    }
}

/// Evaluate blocking detectors on the original text, before any redaction can
/// hide a blocking signal. Reuse the policy's precompiled patterns and return
/// only sanitized text plus content-free rule/count metadata.
#[must_use]
pub fn evaluate_text(text: &str, active: &ActivePolicy) -> FilterOutcome {
    if !active.content_valid {
        return withheld("invalid-content-policy");
    }
    if text.len() > MAX_PROTECTED_CONTENT_BYTES {
        return withheld("content-size-limit");
    }
    if text.contains('\0') {
        return withheld("uninspectable-content");
    }
    if active.redaction.len() + active.blocked_patterns.len() > MAX_REDACTION_RULES
        || active
            .redaction
            .iter()
            .chain(&active.blocked_patterns)
            .any(|(label, pattern)| {
                label.len() > MAX_RULE_LABEL_BYTES
                    || pattern.as_str().len() > MAX_RULE_PATTERN_BYTES
                    || pattern.is_match("")
            })
    {
        return withheld("content-rule-limit");
    }

    for (label, pattern) in &active.blocked_patterns {
        if pattern.is_match(text) {
            return FilterOutcome {
                blocked: true,
                block_reason: Some("content-pattern-blocked".into()),
                audit: vec![(format!("block:{label}"), 1)],
                ..FilterOutcome::default()
            };
        }
    }
    let mut outcome = input_filters::apply(text, &active.filters);
    if outcome.blocked {
        return outcome;
    }
    for (label, pattern) in &active.redaction {
        let marker = format!("[REDACTED:{label}]");
        let mut output = String::with_capacity(outcome.text.len());
        let mut end = 0;
        let mut hits = 0;
        for matched in pattern.find_iter(&outcome.text) {
            let prefix = &outcome.text[end..matched.start()];
            if output.len() + prefix.len() + marker.len() > MAX_PROTECTED_CONTENT_BYTES {
                return withheld("content-size-limit");
            }
            output.push_str(prefix);
            output.push_str(&marker);
            end = matched.end();
            hits += 1;
        }
        let suffix = &outcome.text[end..];
        if output.len() + suffix.len() > MAX_PROTECTED_CONTENT_BYTES {
            return withheld("content-size-limit");
        }
        output.push_str(suffix);
        outcome.text = output;
        if hits > 0 {
            outcome.audit.push((format!("redaction:{label}"), hits));
        }
    }
    // Active protection cannot be disabled by the compression/raw escape hatch
    // or by the user's optional local-secret-redaction setting.
    outcome.text = crate::core::redaction::redact_text(&outcome.text);
    if outcome.text.len() > MAX_PROTECTED_CONTENT_BYTES {
        return withheld("content-size-limit");
    }
    outcome
}

/// Shared storage/transport boundary. Warnings remain caller-owned; never
/// append them to canonical stored content or change a signed payload silently.
pub fn protect_active(text: &str) -> Result<std::borrow::Cow<'_, str>, &'static str> {
    let Some(active) = super::runtime::active() else {
        return Ok(std::borrow::Cow::Borrowed(text));
    };
    let outcome = evaluate_text(text, &active);
    if outcome.blocked {
        Err("content withheld by policy")
    } else {
        Ok(std::borrow::Cow::Owned(outcome.text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(extra: &str) -> ActivePolicy {
        let pack = crate::core::policy::parse(&format!(
            "name = \"content-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{extra}"
        ))
        .expect("valid policy");
        ActivePolicy::from_resolved(crate::core::policy::resolve(&pack).expect("resolved policy"))
    }

    #[test]
    fn customer_numbers_are_redacted_with_content_free_audit() {
        let active = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
        let outcome = evaluate_text("Grüezi 客户 K-482193 / K-482194", &active);
        assert!(!outcome.blocked);
        assert_eq!(
            outcome.text,
            "Grüezi 客户 [REDACTED:customer] / [REDACTED:customer]"
        );
        assert_eq!(outcome.audit, vec![("redaction:customer".into(), 2)]);
    }

    #[test]
    fn named_blocks_see_original_content_before_masking() {
        let active = policy(
            "[redaction]\nmask = 'K-[0-9]{6}'\n[filters.blocked_patterns]\ncustomer = 'K-482193'",
        );
        let blocked = evaluate_text("Contact K-482193", &active);
        assert!(blocked.blocked);
        assert!(blocked.text.is_empty());
        assert_eq!(blocked.audit, vec![("block:customer".into(), 1)]);
        assert!(!format!("{blocked:?}").contains("K-482193"));
        let allowed = evaluate_text("Contact K-482194", &active);
        assert!(!allowed.blocked);
        assert_eq!(allowed.text, "Contact [REDACTED:mask]");
    }

    #[test]
    fn organization_block_cannot_be_replaced_by_same_named_local_pattern() {
        let org = policy("[filters.blocked_patterns]\ncustomer = 'ORG-[0-9]+'").resolved;
        let local = policy("[filters.blocked_patterns]\ncustomer = 'LOCAL-[0-9]+'").resolved;
        let merged = ActivePolicy::from_resolved(crate::core::policy::floor::merge_floor(
            &org,
            Some(&local),
        ));
        assert!(evaluate_text("ORG-17", &merged).blocked);
        assert!(evaluate_text("LOCAL-18", &merged).blocked);
        assert!(!evaluate_text("ordinary context", &merged).blocked);
        let invalid = policy("[filters.blocked_patterns]\ncustomer = 'x*'");
        assert!(evaluate_text("ordinary context", &invalid).blocked);
    }

    #[test]
    fn redaction_cannot_hide_a_classification_block() {
        let active =
            policy("[redaction]\nlabel = 'CONFIDENTIAL'\n[filters]\nclassification = 'block'");
        let outcome = evaluate_text("CONFIDENTIAL\nK-482193", &active);
        assert!(outcome.blocked);
        assert!(outcome.text.is_empty());
        assert!(!format!("{:?}", outcome.audit).contains("K-482193"));
    }

    #[test]
    fn pii_redaction_and_company_rules_both_apply() {
        let active = policy("[redaction]\ncustomer = 'K-[0-9]{6}'\n[filters]\npii = 'redact'");
        let outcome = evaluate_text("K-482193 contact alice@example.com", &active);
        assert!(!outcome.blocked);
        assert!(!outcome.text.contains("K-482193"));
        assert!(!outcome.text.contains("alice@example.com"));
    }

    #[test]
    fn binary_and_oversized_content_are_withheld_without_echo() {
        let active = policy("");
        for text in [
            "customer\0K-482193".to_string(),
            "x".repeat(MAX_PROTECTED_CONTENT_BYTES + 1),
        ] {
            let outcome = evaluate_text(&text, &active);
            assert!(outcome.blocked);
            assert!(outcome.text.is_empty());
        }
    }

    #[test]
    fn empty_matching_rules_are_rejected_without_expansion() {
        let outcome = evaluate_text("K-482193", &policy("[redaction]\nempty = 'x*'"));
        assert!(outcome.blocked);
        assert_eq!(outcome.block_reason.as_deref(), Some("content-rule-limit"));
        assert!(outcome.text.is_empty());
    }

    #[test]
    fn updated_policy_rechecks_previously_released_content() {
        let text = "K-482193";
        assert_eq!(evaluate_text(text, &policy("")).text, text);
        let stricter = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
        assert_eq!(evaluate_text(text, &stricter).text, "[REDACTED:customer]");
    }

    #[test]
    fn invalid_or_excessive_rules_cannot_silently_disappear() {
        for pattern in ["[".to_string(), "x".repeat(MAX_RULE_PATTERN_BYTES + 1)] {
            let mut resolved = policy("").resolved;
            resolved.redaction.insert("invalid".into(), pattern);
            let active = ActivePolicy::from_resolved(resolved);
            assert!(!active.tool_allowed("ctx_read"));
            assert!(evaluate_text("K-482193", &active).blocked);
        }
        let mut resolved = policy("").resolved;
        for n in 0..=MAX_REDACTION_RULES {
            resolved
                .redaction
                .insert(format!("rule{n}"), "customer".into());
        }
        assert!(evaluate_text("customer", &ActivePolicy::from_resolved(resolved)).blocked);
        assert!(evaluate_text("customer", &ActivePolicy::deny_all()).blocked);
    }
}

#[cfg(test)]
#[path = "content_storage_tests.rs"]
mod storage_tests;
