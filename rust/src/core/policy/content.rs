// SPDX-License-Identifier: Apache-2.0
//! Shared, local policy decision before content is released or persisted.
//!
//! Callers retain access/audit and format ownership. In particular, a signed
//! snapshot must reject a rewrite instead of publishing a stale content digest.

use super::runtime::ActivePolicy;
use crate::core::input_filters::{self, FilterAction, FilterConfig, FilterOutcome};
use crate::core::session::EngineSessionId;

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
    evaluate_text_with_filters(text, active, &active.filters)
}

fn evaluate_text_with_filters(
    text: &str,
    active: &ActivePolicy,
    filters: &FilterConfig,
) -> FilterOutcome {
    let mut outcome = evaluate_pack_rules(text, active, filters);
    if outcome.blocked {
        return outcome;
    }
    // Active protection cannot be disabled by the compression/raw escape hatch
    // or by the user's optional local-secret-redaction setting.
    outcome.text = crate::core::redaction::redact_text(&outcome.text);
    if outcome.text.len() > MAX_PROTECTED_CONTENT_BYTES {
        return withheld("content-size-limit");
    }
    // Filtering took time: nothing is handed back as permitted under a
    // policy whose signed horizon passed meanwhile.
    if !active.within_horizon() {
        return withheld(EXPIRED);
    }
    outcome
}

/// Reason for content withheld because the policy's signed horizon passed.
const EXPIRED: &str = "expired-content-policy";

/// What the release point may do with one finished text under the pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReleaseCheck {
    /// The pack leaves the text exactly as it is.
    Unchanged,
    /// No rule of the pack blocks or rewrites the text; only the built-in
    /// secret mask is outstanding, and the masked text is one the pack then
    /// leaves exactly as it is.
    BuiltinSecretOnly,
    /// A rule of the pack blocks the text or would rewrite it.
    Refused,
}

/// Tell a text that only needs the built-in secret mask from one a rule of the
/// pack itself objects to. The first may be masked and delivered where the
/// position allows a rewrite; the second is never rewritten at the release
/// point (a finished result the pack would change is stale).
pub(crate) fn release_check(text: &str, active: &ActivePolicy) -> ReleaseCheck {
    let own = evaluate_pack_rules(text, active, &active.filters);
    if own.blocked || own.text != text {
        return ReleaseCheck::Refused;
    }
    let masked = crate::core::redaction::redact_text(text);
    if masked == text {
        // The pack's rules ran above; the release itself is decided now.
        return if active.within_horizon() {
            ReleaseCheck::Unchanged
        } else {
            ReleaseCheck::Refused
        };
    }
    // Like with like: the masked text, evaluated in full, must be a fixed
    // point — no rule of the pack fires on it and a second mask changes
    // nothing. Otherwise the result cannot be released in a settled form.
    let settled = evaluate_text(&masked, active);
    if settled.blocked || settled.text != masked {
        return ReleaseCheck::Refused;
    }
    ReleaseCheck::BuiltinSecretOnly
}

/// The pack's own rules: blocking detectors on the original text, then the
/// pack's filters and `[redaction]` entries. Without the built-in secret mask.
fn evaluate_pack_rules(text: &str, active: &ActivePolicy, filters: &FilterConfig) -> FilterOutcome {
    if !active.content_valid {
        return withheld("invalid-content-policy");
    }
    // Before any rule is consulted: a policy past its signed horizon decides
    // nothing, however it was obtained and however long it was kept.
    if !active.within_horizon() {
        return withheld(EXPIRED);
    }
    if text.len() > MAX_PROTECTED_CONTENT_BYTES {
        return withheld("content-size-limit");
    }
    if text.contains('\0') {
        return withheld("uninspectable-content");
    }
    if active.redaction.len() + active.blocked_patterns.len()
        > MAX_REDACTION_RULES
            + active
                .hive_rules
                .min(crate::core::hive::policy::HIVE_MAX_RULES)
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
    let mut outcome = input_filters::apply(text, filters);
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
    if !active.within_horizon() {
        return withheld(EXPIRED);
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

/// Protect a validated Engine identity with project rules while skipping only
/// the project PII detector, which can mistake the legacy timestamp format for
/// a payment card. Explicit block/redaction rules remain active.
pub(crate) fn protect_active_engine_identity(
    id: EngineSessionId<'_>,
) -> Result<std::borrow::Cow<'_, str>, &'static str> {
    let text = id.as_str();
    let Some(active) = super::runtime::active() else {
        return Ok(std::borrow::Cow::Borrowed(text));
    };
    protect_engine_identity_with_active(id, &active)
}

pub(crate) fn protect_engine_identity_with_active<'a>(
    id: EngineSessionId<'a>,
    active: &ActivePolicy,
) -> Result<std::borrow::Cow<'a, str>, &'static str> {
    let text = id.as_str();
    let filters = FilterConfig::new(
        FilterAction::Off,
        active.filters.classification,
        active.filters.injection,
        &active.resolved.filters.blocked_labels,
    );
    let outcome = evaluate_text_with_filters(text, active, &filters);
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
        assert!(blocked.text.is_empty(), "{:?}", blocked.text);
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
        assert!(outcome.text.is_empty(), "{:?}", outcome.text);
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
            assert!(outcome.text.is_empty(), "{:?}", outcome.text);
        }
    }

    #[test]
    fn empty_matching_rules_are_rejected_without_expansion() {
        let outcome = evaluate_text("K-482193", &policy("[redaction]\nempty = 'x*'"));
        assert!(outcome.blocked);
        assert_eq!(outcome.block_reason.as_deref(), Some("content-rule-limit"));
        assert!(outcome.text.is_empty(), "{:?}", outcome.text);
    }

    #[test]
    fn updated_policy_rechecks_previously_released_content() {
        let text = "K-482193";
        assert_eq!(evaluate_text(text, &policy("")).text, text);
        let stricter = policy("[redaction]\ncustomer = 'K-[0-9]{6}'");
        assert_eq!(evaluate_text(text, &stricter).text, "[REDACTED:customer]");
    }

    #[test]
    fn engine_identity_skips_only_generic_pii_and_still_honors_project_blocks() {
        let id_text = "20261004-123406-123456p43210s0";
        let id = crate::core::session::parse_engine_session_id(id_text).expect("legacy ID");
        let pii_policy = policy("[filters]\npii = 'redact'");
        // The standard Luhn-valid test number in groups of four, assembled at
        // run time so the source carries no card-shaped literal.
        let group = "1".repeat(4);
        let card = format!("4{} {group} {group} {group}", "1".repeat(3));
        let generic = evaluate_text(&format!("paid with {card}"), &pii_policy);
        assert!(
            !generic.blocked && !generic.text.contains(card.as_str()),
            "free text stays under the PII detector"
        );
        // Changed expectation: this asserted that the id itself is masked in
        // free text. Since the 3.11 detector rule (`pii::card_layout_plausible`:
        // a separated digit run is a card only in an issuer layout) its
        // `YYYYmmdd-HHMMSS` run is no card, and the id is delivered as written.
        assert_eq!(evaluate_text(id_text, &pii_policy).text.as_str(), id_text);
        assert_eq!(
            protect_engine_identity_with_active(id, &pii_policy)
                .expect("typed identity remains intact")
                .as_ref(),
            id_text
        );

        let blocked_policy =
            policy("[filters]\npii = 'redact'\n[filters.blocked_patterns]\nlegacy_id = '123406'");
        assert!(
            protect_engine_identity_with_active(id, &blocked_policy).is_err(),
            "project blocked_patterns must still reject typed IDs"
        );
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
pub(crate) mod storage_tests;
