// SPDX-License-Identifier: Apache-2.0
//! GHSA-xmx8-69qv-7f86: the secret detector and the redactor must judge the
//! same value of a key/value or comparison expression. When they read
//! different spans (the operand with or without its operator, with or
//! without a trailing `)` or `:`), admission delivered a value the detector
//! had just flagged, or withheld a benign comparison.
//!
//! Key names are assembled at run time and values are split with `concat!`,
//! so the repository's own scanners never see a literal credential line.

use lean_ctx_protocol::context_gateway::ContextDispositionV1;

use super::*;

/// The public corpus value of `secret_detection::tests::detects_generic_api_key`.
const VALUE: &str = concat!("abcdefghijklmnop", "qrstuvwxyz1234567890");

fn operands() -> [String; 4] {
    [
        VALUE.to_owned(),
        concat!("q7RtZx91Lm2N", "p8Vb3Kc6Hd4Fg").to_owned(),
        concat!("svc_", "9f8e7d6c5b4a3f2e1d0c").to_owned(),
        concat!("A1b2C3d4E5f6G7h8", "I9j0K1l2M3n4").to_owned(),
    ]
}

fn name() -> String {
    ["tok", "en"].concat()
}

fn admit_project(text: &str) -> Admission {
    admit(
        text,
        None,
        &origin::SourceClass::Project,
        &AdmissionPolicy::builtin_for_tests(
            ContextGatewayConfig::default(),
            SecretDetectionConfig::default(),
        ),
    )
}

fn hits(text: &str) -> usize {
    detectors::secret_hits_for_bench(text, &SecretDetectionConfig::default())
}

/// The security property: whatever admission delivers carries no value the
/// secret detector reports.
fn assert_no_flagged_value_delivered(text: &str) -> Admission {
    let admission = admit_project(text);
    if let Some(out) = &admission.text {
        assert_eq!(
            hits(out),
            0,
            "delivered a flagged value: {text:?} -> {out:?} ({:?})",
            admission.decision.disposition
        );
    }
    admission
}

#[test]
fn unchanged_output_after_a_detection_is_never_delivered() {
    let n = name();
    for text in [
        format!("log: {n}={VALUE})\n"),
        format!("{n}={VALUE}:\n"),
        format!("{n}={VALUE}]\n"),
        format!("if ({n}=={VALUE}):\n"),
        format!("log: {n}=={VALUE}\n"),
        format!("{n}:={VALUE}\n"),
    ] {
        assert!(hits(&text) > 0, "the corpus value is a detection: {text:?}");
        let admission = assert_no_flagged_value_delivered(&text);
        assert!(
            admission
                .text
                .as_deref()
                .is_none_or(|out| !out.contains(VALUE)),
            "{text:?} delivered the flagged value ({:?})",
            admission.decision.disposition
        );
    }
}

/// A second value swallowed by a benign first value is invisible to the
/// redactor (its matches do not overlap) but reported by the detector:
/// admission re-checks the unchanged text and withholds it.
#[test]
fn a_value_hidden_behind_a_benign_one_is_withheld() {
    let n = name();
    let short = concat!("q7RtZx91Lm2N", "p8Vb3Kc6Hd4F");
    let text = format!("if ({n}=abc)||({n}=={short}):\n");
    assert!(hits(&text) > 0, "the hidden value is a detection: {text:?}");
    let admission = assert_no_flagged_value_delivered(&text);
    assert!(
        admission
            .text
            .as_deref()
            .is_none_or(|out| !out.contains(short)),
        "{text:?} delivered the hidden value ({:?})",
        admission.decision.disposition
    );
}

/// A placeholder password in a URL is no credential to either layer, so the
/// text is delivered unchanged instead of withheld whole.
#[test]
fn a_placeholder_url_password_is_delivered_unchanged() {
    for text in [
        concat!(
            "Looking in indexes: https://user:",
            "****",
            "@host/simple\n"
        ),
        concat!(
            "git clone https://oauth2:",
            "<token>",
            "@gitlab.com/x/y.git\n"
        ),
        concat!("curl https://user:", "change", "me@localhost\n"),
    ] {
        assert_eq!(hits(text), 0, "{text:?}");
        let admission = assert_no_flagged_value_delivered(text);
        assert_eq!(
            admission.decision.disposition,
            ContextDispositionV1::Allow,
            "{text:?}: {:?}",
            admission.decision.reason_codes
        );
        assert_eq!(admission.text.as_deref(), Some(text));
    }
}

#[test]
fn a_comparison_operand_is_masked_like_its_assignment() {
    let n = name();
    let mut rows = 0;
    for operand in operands() {
        let assignment = format!("{n}={operand}\n");
        let masked = admit_project(&assignment);
        assert!(
            masked
                .text
                .as_deref()
                .is_none_or(|out| !out.contains(&operand)),
            "the assignment form is masked: {assignment:?}"
        );
        for op in ["=", "==", ":=", "==="] {
            for prefix in ["", "if (", "log: ", "x && "] {
                for suffix in ["", ")", ":", "):", "]", "}", ";"] {
                    let text = format!("{prefix}{n}{op}{operand}{suffix}\n");
                    let admission = assert_no_flagged_value_delivered(&text);
                    assert!(
                        admission
                            .text
                            .as_deref()
                            .is_none_or(|out| !out.contains(&operand)),
                        "{text:?} delivered its operand ({:?})",
                        admission.decision.disposition
                    );
                    rows += 1;
                }
            }
        }
    }
    assert_eq!(rows, 448);
}

#[test]
fn both_layers_judge_one_operand() {
    use crate::core::redaction::{is_benign_quoted_value, is_benign_secret_value};
    // The redactor's span and the detector's span of one value agree.
    for value in operands() {
        for span in [
            value.clone(),
            format!("={value}"),
            format!("=={value}"),
            format!("{value})"),
            format!("={value}):"),
            format!("{value}:"),
            format!("{value}]"),
            format!("{value}(arg)"),
        ] {
            assert!(!is_benign_secret_value(&span), "{span:?}");
        }
        assert!(!is_benign_quoted_value(&format!("{value})")));
    }
    for span in [
        "=",
        "==",
        "=unit_resource_reference",
        "=unit_resource_reference):",
        "unit_resource_reference)",
        "getValue())",
        "Promise<string>",
        "Option<String>,",
        "=config.reference)",
        "<your-value-here>",
    ] {
        assert!(is_benign_secret_value(span), "{span:?}");
    }
}

#[test]
fn only_the_flagged_run_is_masked_and_its_syntax_survives() {
    let n = name();
    for (text, kept) in [
        (format!("left|{n}={VALUE}|right\n"), "|right"),
        (format!("{n}={VALUE}(arg)\n"), "(arg)"),
        (format!("if ({n}=={VALUE}(arg)):\n"), "=="),
    ] {
        let out = crate::core::redaction::redact_text(&text);
        assert!(!out.contains(VALUE), "{text:?} -> {out:?}");
        assert!(out.contains(kept), "{text:?} lost {kept:?}: {out:?}");
        assert_eq!(hits(&out), 0, "{text:?} -> {out:?}");
    }
}

#[test]
fn a_benign_comparison_is_delivered_unchanged() {
    let n = name();
    for text in [
        format!("if ({n}==unit_resource_reference):\n    pass\n"),
        format!("if {n} == other_reference:\n"),
        format!("assert({n}===config.reference)\n"),
        format!("fn main() {{}}\nif ({n}==unit_resource_reference):\n"),
    ] {
        let admission = assert_no_flagged_value_delivered(&text);
        assert_eq!(
            admission.decision.disposition,
            ContextDispositionV1::Allow,
            "{text:?}: {:?}",
            admission.decision.reason_codes
        );
        assert_eq!(admission.text.as_deref(), Some(text.as_str()));
    }
}
