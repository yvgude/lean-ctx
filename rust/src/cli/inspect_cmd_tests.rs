// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn every_reason_the_gateway_emits_has_a_sentence() {
    for code in [
        "secret.redacted",
        "secret.flagged",
        "secret.blocked",
        "pii.redacted",
        "pii.flagged",
        "pii.blocked",
        "injection.flagged",
        "injection.redacted",
        "injection.blocked",
        "classification.flagged",
        "classification.blocked",
        "coverage.partial",
        "coverage.budget_exhausted",
        "coverage.unsupported_media",
        "detector.timed_out",
        "detector.invalid_pattern",
        "redaction.incomplete",
        "semantic.unavailable",
        "semantic.partial",
        "semantic.warming",
        "semantic.categories_not_inspected",
        "semantic.language_not_covered",
        "semantic.language_undetermined",
        "semantic.document_type_not_covered",
        "semantic.coverage_unknown",
        "semantic.language_coverage_unknown",
        "semantic.document_type_coverage_unknown",
        "gateway.disabled",
        "destination.remote_allowed_once",
        "destination.remote_restricted",
        "egress.sealed_content",
        "egress.opaque_payload",
    ] {
        assert!(
            !explain(code).starts_with("see docs"),
            "{code} needs an explanation"
        );
    }
    assert_eq!(
        explain("semantic.prompt_injection.flagged"),
        "semantic detector: prompt injection flagged"
    );
    assert!(explain("semantic.pii.not_inspected").contains("did not examine pii"));
    // Coverage reasons have their own sentence, not the generic fallback.
    for code in [
        "semantic.language_not_covered",
        "semantic.language_undetermined",
        "semantic.document_type_not_covered",
        "semantic.coverage_unknown",
        "semantic.language_coverage_unknown",
        "semantic.document_type_coverage_unknown",
    ] {
        assert!(!explain(code).starts_with("semantic detector:"), "{code}");
    }
}

#[test]
fn arguments_are_validated() {
    assert!(parse(&["--last".into(), "0".into()]).is_err());
    assert!(parse(&["--bogus".into()]).is_err());
    assert!(matches!(parse(&["--help".into()]), Ok(None)));
    let options = parse(&["--last".into(), "3".into(), "--json".into()])
        .expect("valid arguments")
        .expect("not help");
    assert_eq!((options.last, options.json), (3, true));
    assert!(parse(&["--task".into()]).is_err());
    assert!(parse(&["--task".into(), String::new()]).is_err());
    let options = parse(&["--task".into(), "task-1".into()])
        .expect("valid arguments")
        .expect("not help");
    assert_eq!(options.task.as_deref(), Some("task-1"));
    let options = parse(&[
        "--task".into(),
        "task-1".into(),
        "--project-id".into(),
        "project-a".into(),
    ])
    .expect("valid arguments")
    .expect("not help");
    assert_eq!(
        options
            .project_id
            .map(|project| project.as_str().to_owned()),
        Some("project-a".to_owned())
    );
    assert!(
        parse(&["--project-id".into(), "project-a".into()]).is_err(),
        "a scope without a task"
    );
}

#[test]
fn a_lineage_names_its_gaps() {
    let lineage = crate::core::context_store::lineage::build(
        "task-1",
        Ok(Vec::new()),
        crate::core::context_store::lineage::LedgerScope::Verified,
        &crate::core::context_admission::receipt_store::TaskReceipts {
            entries: Vec::new(),
            index_complete: true,
        },
    );
    let text = render_lineage(&lineage);
    assert!(text.starts_with("Task task-1 — outcome unknown"), "{text}");
    assert!(text.contains("gaps       no_plan_recorded"), "{text}");
}

#[test]
fn an_empty_project_says_so() {
    assert!(render_human("/p", &[], None).contains("No Context Gateway receipts"));
}

#[test]
fn unverifiable_receipts_are_named_not_shown() {
    let receipts = [("ab".repeat(32), Err(LoadError::Tampered))];
    let text = render_human("/p", &receipts, None);
    assert!(text.contains("NOT VERIFIED (tampered)"), "{text}");
}

/// G08: delivered bytes lead back to the receipt that binds them — also
/// when the host dropped the final newline — and the trace that is shown
/// names the output digest and the principal, never the bytes.
#[test]
fn delivered_bytes_lead_back_to_their_receipt() {
    use crate::core::context_admission::capture::{ADMISSIONS, AdmissionCapture, Delivered};
    use crate::core::context_admission::receipt_store::{ReceiptRecord, ReceiptSubject, observed};

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    // Protection: the receipt gets its trace sidecar and audit anchor.
    let config = crate::core::paths::config_dir_read_only().expect("config dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("config");
    let duty = receipt_store::Duty::at_entry();
    let delivery = trace::Delivery::mcp("ctx_read", duty.snapshot());
    let delivered = "pub fn traced_body() {}\n";
    let capture = AdmissionCapture::for_call(duty.snapshot().clone());
    ADMISSIONS
        .sync_scope(Some(capture.clone()), || {
            crate::core::context_admission::admit_context_source(delivered)
        })
        .expect("a clean source is admitted");
    let record = receipt_store::record_delivery(
        &capture,
        &ReceiptSubject {
            tool: "ctx_read",
            agent_id: Some("claude-7"),
            presenter_ref: None,
            project_root: "/p",
            delivery: &delivery,
        },
        &Delivered {
            text: delivered,
            is_error: false,
            tokens: 6,
        },
        &duty,
    );
    let ReceiptRecord::Stored { digest, .. } = &record else {
        panic!("the receipt is stored: {record:?}");
    };

    let (lookup, found) = find_by_output("/another-project", b"pub fn traced_body() {}");
    assert_eq!(
        lookup.matched.as_ref().map(|(form, _)| *form),
        Some("with a final newline")
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, digest.hex());

    let text = render_human("/p", &found, Some(&AuditAnchors::load()));
    let output = receipt_store::output_digest(delivered.as_bytes());
    assert!(text.contains(output.as_str()), "{text}");
    assert!(text.contains("agent:claude-7"), "{text}");
    assert!(!text.contains("traced_body"), "{text}");
    // The sidecar is bound to the receipt and to its audit anchor; an
    // altered sidecar is a finding, never shown as if it were intact.
    assert!(
        text.contains("stored bytes match the audit anchor"),
        "{text}"
    );
    assert!(text.contains("transport  mcp · ctx_read"), "{text}");
    // The V1 outcome is shown as recorded at preparation; nothing was
    // observed yet, and the inspector says exactly that.
    assert!(
        text.contains("delivered (recorded at preparation"),
        "{text}"
    );
    assert!(text.contains("observed   unconfirmed"), "{text}");
    // No per-source access check was recorded: the inspector says the
    // right to read is not proven instead of implying it (E07).
    assert!(text.contains("rights     not recorded"), "{text}");
    // Doc 19: the same receipt as one lineage — the MCP request it
    // answered, its decision in the five-outcome vocabulary, prepared.
    let stored = receipt_store::load(digest.hex()).expect("stored receipt");
    let lineage = receipt_store::view::lineage(&stored).expect("lineage of an MCP receipt");
    use lean_ctx_protocol::context_lineage::{
        ContextDecisionOutcomeV1, ContextReceiptStateV1, IntegrationRouteV1,
    };
    assert_eq!(lineage.request.route, IntegrationRouteV1::Mcp);
    assert_eq!(lineage.decision_receipt.as_str(), digest.as_str());
    assert_eq!(lineage.receipt_state, ContextReceiptStateV1::Prepared);
    assert!(
        lineage
            .outcomes
            .iter()
            .all(|o| o.outcome == ContextDecisionOutcomeV1::Allow)
    );
    // A second call prepares the SAME bytes and returns first. Its
    // observation is written through its own handle and names its own
    // receipt; the first call, which has not returned, stays unconfirmed.
    let twin_capture = AdmissionCapture::for_call(duty.snapshot().clone());
    ADMISSIONS
        .sync_scope(Some(twin_capture.clone()), || {
            crate::core::context_admission::admit_context_source(delivered)
        })
        .expect("a clean source is admitted");
    let twin = receipt_store::record_delivery(
        &twin_capture,
        &ReceiptSubject {
            tool: "ctx_read",
            agent_id: None,
            presenter_ref: None,
            project_root: "/twin",
            delivery: &delivery,
        },
        &Delivered {
            text: delivered,
            is_error: false,
            tokens: 6,
        },
        &duty,
    );
    let ReceiptRecord::Stored {
        digest: twin_digest,
        ..
    } = &twin
    else {
        panic!("the second receipt is stored: {twin:?}");
    };
    assert_ne!(twin_digest, digest, "two calls, two receipts");
    let twin_handle = twin.clone().into_prepared().expect("sealed");
    let other = receipt_store::output_digest(b"other bytes");
    assert!(!twin_handle.returned_to_transport(&other), "other bytes");
    assert!(twin_handle.returned_to_transport(&output));
    assert!(matches!(
        observed::load(twin_digest),
        observed::StoredObservation::Bound {
            observed: observed::Observed::ReturnedToTransport { .. },
            ..
        }
    ));
    assert_eq!(
        observed::load(digest),
        observed::StoredObservation::Absent,
        "the call that has not returned is not confirmed by another call's bytes"
    );
    // Now the first handler returns its result to the transport:
    // observed as that, never as a delivery to the host — and shown as
    // a local, unsigned record next to the signed, prepared receipt.
    let handle = record.clone().into_prepared().expect("sealed");
    assert!(handle.returned_to_transport(&output));
    let text = render_human("/p", &found, Some(&AuditAnchors::load()));
    assert!(text.contains("observed   returned to transport"), "{text}");
    let stored = receipt_store::load(digest.hex()).expect("stored receipt");
    let lineage = receipt_store::view::lineage(&stored).expect("lineage");
    // Returned to the MCP transport: handed off, and never model egress.
    assert_eq!(lineage.receipt_state, ContextReceiptStateV1::HandedOff);
    let json = render_json(&found, &AuditAnchors::load(), None);
    assert!(json.contains("\"receipt_state\": \"handed_off\""), "{json}");
    assert!(!json.contains("observed_model_egress"), "{json}");
    assert!(
        text.contains("receipt by the host is not observable"),
        "{text}"
    );
    assert!(
        text.contains("observation: local unsigned record"),
        "{text}"
    );
    assert!(text.contains("(link verified)"), "{text}");
    let sidecar = crate::core::data_dir::lean_ctx_data_dir()
        .expect("data dir")
        .join("gateway")
        .join("traces")
        .join(format!("{}.json", digest.hex()));
    let altered = std::fs::read_to_string(&sidecar)
        .expect("stored sidecar")
        .replace("\"mcp\"", "\"cli\"");
    std::fs::write(&sidecar, altered).expect("altered sidecar");
    let text = render_human("/p", &found, Some(&AuditAnchors::load()));
    assert!(text.contains("ALTERED"), "{text}");

    // A receipt that was stored while its audit anchor then failed: the
    // delivery is withheld, and the stored V1 receipt — which still says
    // `delivered` — is shown as a failed preparation, not as a delivery.
    let trail = crate::core::audit_trail::default_trail_path().expect("trail path");
    std::fs::remove_file(&trail).expect("the trail of the first receipt");
    std::fs::create_dir_all(&trail).expect("a directory where the trail file would be");
    let failing = "pub fn unanchored_body() {}\n";
    let capture = AdmissionCapture::for_call(duty.snapshot().clone());
    ADMISSIONS
        .sync_scope(Some(capture.clone()), || {
            crate::core::context_admission::admit_context_source(failing)
        })
        .expect("a clean source is admitted");
    let record = receipt_store::record_delivery(
        &capture,
        &ReceiptSubject {
            tool: "ctx_read",
            agent_id: None,
            presenter_ref: None,
            project_root: "/p",
            delivery: &delivery,
        },
        &Delivered {
            text: failing,
            is_error: false,
            tokens: 6,
        },
        &duty,
    );
    assert!(record.withholds(duty.mandatory), "{record:?}");
    let (_, found) = find_by_output("/p", failing.as_bytes());
    assert_eq!(found.len(), 1, "the receipt itself was stored");
    let text = render_human("/p", &found, None);
    assert!(
        text.contains("prepared, NOT delivered: preparation failed"),
        "{text}"
    );
    assert!(text.contains("step: anchor"), "{text}");
    assert!(!text.contains("handed over"), "{text}");

    let (lookup, found) = find_by_output("/p", b"never delivered");
    assert!(lookup.matched.is_none() && found.is_empty());
}
