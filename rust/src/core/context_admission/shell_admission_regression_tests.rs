// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn rendered_notice_uses_a_newline_and_survives_empty_text() {
    let admission = ToolOutputAdmission {
        text: Some("body".to_string()),
        notice: Some("[notice]".to_string()),
    };
    assert_eq!(admission.rendered(), "[notice]\nbody");

    let empty = ToolOutputAdmission {
        text: Some(String::new()),
        notice: Some("[notice]".to_string()),
    };
    assert_eq!(empty.rendered(), "[notice]");
}

#[test]
fn incomplete_capture_is_never_persistable() {
    let policy = AdmissionPolicy::builtin(
        ContextGatewayConfig::default(),
        SecretDetectionConfig::default(),
    );
    let incomplete = OutputCompleteness {
        reader_incomplete: true,
        ..OutputCompleteness::complete()
    };
    assert!(
        admit_output_for_storage("partial output", incomplete, &policy).is_none(),
        "an incomplete prefix must not seed a cache"
    );
}

/// #2164: a background job whose command exited while a process it left
/// running still holds the pipe keeps its captured output for the status
/// poll; every other gap, and a mandatory gateway, still withholds it.
#[test]
fn background_poll_keeps_a_capture_whose_only_gap_is_a_held_pipe() {
    let developer = AdmissionPolicy::builtin(
        ContextGatewayConfig::default(),
        SecretDetectionConfig::default(),
    );
    let held_pipe = OutputCompleteness {
        reader_incomplete: true,
        ..OutputCompleteness::complete()
    };
    assert_eq!(
        admit_output_for_poll("hello stdout\nboom stderr", held_pipe, &developer).as_deref(),
        Some("hello stdout\nboom stderr")
    );
    for other_gap in [
        OutputCompleteness {
            truncated: true,
            ..held_pipe
        },
        OutputCompleteness {
            timed_out: true,
            ..held_pipe
        },
        OutputCompleteness {
            cancelled: true,
            ..OutputCompleteness::complete()
        },
    ] {
        assert!(admit_output_for_poll("partial", other_gap, &developer).is_none());
    }

    let governed = AdmissionPolicy::builtin(
        ContextGatewayConfig {
            mode: GatewayModeV1::Governed,
            ..ContextGatewayConfig::default()
        },
        SecretDetectionConfig::default(),
    );
    if governed.enabled_effective() {
        assert!(admit_output_for_poll("partial", held_pipe, &governed).is_none());
    }
}

#[test]
fn mandatory_gateway_marks_and_withholds_incomplete_capture() {
    let gateway = ContextGatewayConfig {
        mode: GatewayModeV1::Governed,
        ..ContextGatewayConfig::default()
    };
    let policy = AdmissionPolicy::builtin(gateway, SecretDetectionConfig::default());
    let incomplete = OutputCompleteness {
        truncated: true,
        ..OutputCompleteness::complete()
    };
    let mut admission = admit(
        "partial output",
        None,
        &origin::SourceClass::Project,
        &policy,
    );
    mark_capture_incomplete(&mut admission, incomplete, &policy);

    if policy.enabled_effective() {
        assert!(admission.text.is_none());
        assert!(
            admission
                .decision
                .reason_codes
                .iter()
                .any(|code| code.as_str() == "coverage.output_partial")
        );
    }
}

/// AWS's documented example access key, assembled so the source carries
/// no literal the public secret scanner flags.
fn aws_documentation_key() -> String {
    ["AK", "IA", "IOSFODNN7EXAMPLE"].concat()
}

#[test]
fn overlapping_policy_and_gateway_redactions_withhold_noncomposable_output() {
    let pack = crate::core::policy::parse(&format!(
        "name = \"composition-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n[redaction]\ncustomer = 'customer {}'",
        aws_documentation_key()
    ))
    .expect("valid policy");
    let _project_policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(
        crate::core::policy::resolve(&pack).expect("resolved policy"),
    ));
    let original = format!("customer {}", aws_documentation_key());
    let original = original.as_str();
    let project_original = crate::core::policy::content::protect_active(original)
        .expect("project redaction is allowed")
        .into_owned();
    let policy = AdmissionPolicy::builtin(
        ContextGatewayConfig::default(),
        SecretDetectionConfig::default(),
    );
    let gateway_output = admit(original, None, &origin::SourceClass::Project, &policy)
        .text
        .expect("developer gateway redacts and delivers");
    assert_ne!(project_original.as_str(), original);
    assert_ne!(gateway_output.as_str(), original);
    assert!(
        compose_active_gateway_output(
            &project_original,
            &gateway_output,
            &origin::SourceClass::Project,
            &policy,
            OutputCompleteness::complete(),
        )
        .is_none(),
        "overlapping redactions that depend on order must be withheld"
    );
}
