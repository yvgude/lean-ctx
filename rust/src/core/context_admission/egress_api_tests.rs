// SPDX-License-Identifier: Apache-2.0
//! The shared egress API the proxy builds one receipt from: joined outcomes,
//! refusals that carry their deny decision, project-bound opaque admission and
//! the policy epoch. Credential fixtures are split with `concat!` so the
//! repository's own secret scanners never see a literal key.

use lean_ctx_protocol::context_gateway::{
    ClassificationV1, ContextDispositionV1, EgressAdmissionOutcomeV1, ReasonCodeV1,
};
use serde_json::json;

use super::AdmissionPolicy;
use super::egress::{
    self, EgressBody, EgressTarget, TrustedDestinationBinding,
    admit_opaque_for_project_with_binding, admit_request,
};
use crate::core::intelligence_runtime::semantic_detectors::SemanticReport;

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");

const REMOTE: EgressTarget<'static> = EgressTarget {
    provider: "Anthropic",
    model: Some("claude-sonnet-5-5"),
    upstream_base: "https://api.anthropic.com",
};

const LOCAL: EgressTarget<'static> = EgressTarget {
    provider: "OpenAI",
    model: Some("llama"),
    upstream_base: "http://127.0.0.1:11434",
};

fn isolated(config: &str) -> crate::core::data_dir::IsolatedDataDir {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    data
}

fn user(text: &str) -> serde_json::Value {
    json!({"model": "m", "messages": [{"role": "user", "content": text}]})
}

fn code(value: &str) -> ReasonCodeV1 {
    ReasonCodeV1::new(value).expect("valid reason code")
}

/// One receipt for the caller-content admission and the final-content
/// admission: redaction provenance of the first, decision of the second, the
/// higher classification.
#[test]
#[serial_test::serial(cache_telemetry)]
fn joined_outcome_keeps_original_provenance_and_the_final_decision() {
    let _data = isolated("");
    let original = user(&format!("*** TOP SECRET ***\ndeploy with {AWS_KEY}"));
    // The sequence of the proxy adapter: admit the caller's content, transform
    // the admitted body (here: a compression that drops the marking line and
    // rewrites the sentence), admit the final content, join, one receipt.
    let caller = admit_request(&original, &LOCAL);
    let EgressBody::Rewritten(admitted) = &caller.body else {
        panic!("the caller content is masked: {:?}", caller.body);
    };
    assert!(!admitted.to_string().contains(AWS_KEY));
    assert_eq!(caller.classification, Some(ClassificationV1::Restricted));
    let final_body = user("deploy with the configured credentials");
    let final_bytes = serde_json::to_vec(&final_body).expect("final bytes");
    let later = admit_request(&final_body, &LOCAL);
    assert!(matches!(later.body, EgressBody::Unchanged));

    let joined = caller.join(later);
    assert!(
        matches!(joined.body, EgressBody::Unchanged),
        "the body is the final admission's"
    );
    assert_eq!(
        joined.classification,
        Some(ClassificationV1::Restricted),
        "the joined classification is the higher one"
    );
    let receipt = egress::finish_admission(&joined, Some(&final_bytes), final_bytes.len(), None)
        .expect("receipt")
        .expect("evidence")
        .decision_receipt
        .expect("decision receipt");
    assert_eq!(
        receipt.decisions.len(),
        2,
        "both decision sets, one receipt"
    );
    assert!(receipt.security.redactions > 0, "redaction provenance kept");
    assert!(receipt.decisions.iter().any(|decision| {
        decision
            .reason_codes
            .iter()
            .any(|reason| reason.as_str() == "secret.redacted")
    }));
    assert!(
        receipt.final_context.is_some(),
        "the final content was prepared"
    );

    // A refusal in the caller-content admission refuses the joined outcome.
    let refused = admit_request(&original, &REMOTE).join(admit_request(&final_body, &REMOTE));
    assert!(matches!(refused.body, EgressBody::Refused(_)));
    assert!(
        refused
            .refusal_codes()
            .contains(&code("destination.remote_restricted"))
    );
}

/// Outcomes of different epochs are never merged: the result is a typed
/// refusal whose receipt carries `policy.changed`, not a joined outcome.
#[test]
#[serial_test::serial(cache_telemetry)]
fn outcomes_of_different_policy_epochs_are_not_joined() {
    let _data = isolated("");
    let body = user(&format!("deploy with {AWS_KEY}"));
    let bytes = serde_json::to_vec(&body).expect("bytes");
    for (earlier, later) in [
        // Another project fingerprint.
        (
            egress::admit_request_for_project(&body, &LOCAL, b"project-a"),
            egress::admit_request_for_project(&body, &LOCAL, b"project-b"),
        ),
        // A project-bound admission and a global one.
        (
            egress::admit_request_for_project(&body, &LOCAL, b"project-a"),
            admit_request(&body, &LOCAL),
        ),
        // The environment switch moved between the two admissions.
        (admit_request(&body, &LOCAL), {
            let _off = KillSwitchOff::set();
            admit_request(&body, &LOCAL)
        }),
    ] {
        let joined = earlier.join(later);
        assert!(
            matches!(joined.body, EgressBody::Refused(_)),
            "{:?}",
            joined.body
        );
        assert_eq!(joined.refusal_codes(), [code("policy.changed")]);
        let receipt = egress::finish_admission(&joined, Some(&bytes), bytes.len(), None)
            .expect("receipt")
            .expect("evidence")
            .decision_receipt
            .expect("decision receipt");
        assert!(receipt.final_context.is_none(), "nothing is prepared");
    }
    // Control: identical epochs with a project fingerprint do join.
    let joined = egress::admit_request_for_project(&body, &LOCAL, b"project-a").join(
        egress::admit_request_for_project(&body, &LOCAL, b"project-a"),
    );
    assert!(matches!(joined.body, EgressBody::Rewritten(_)));
}

/// The fingerprint and every decision of one admission use ONE snapshot. The
/// environment is moved at the barrier right after the snapshot is taken —
/// between "fingerprint" and "decision" — and back before the caller asks
/// `is_current` (A → B → A). Before, the decision re-read the environment and
/// could be taken under B while the epoch still matched A.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_transient_environment_change_cannot_split_fingerprint_and_decision() {
    let _data = isolated("");
    let body = user(&format!("deploy with {AWS_KEY}"));

    // A = on. B = off, exactly while the admission decides.
    egress::failpoint::arm(|| crate::test_env::set_var("LEAN_CTX_CONTEXT_GATEWAY", "off"));
    let outcome = admit_request(&body, &REMOTE);
    crate::test_env::remove_var("LEAN_CTX_CONTEXT_GATEWAY");
    let EgressBody::Rewritten(admitted) = &outcome.body else {
        panic!("decided under the snapshot (on): {:?}", outcome.body);
    };
    assert!(!admitted.to_string().contains(AWS_KEY));
    assert!(outcome.policy_epoch().is_current(None), "A again: current");

    // A = off. B = on while deciding: the decision is the uninspected
    // passthrough of A, and its epoch says so — it is not current while the
    // live environment is on.
    let off = KillSwitchOff::set();
    egress::failpoint::arm(|| crate::test_env::remove_var("LEAN_CTX_CONTEXT_GATEWAY"));
    let outcome = admit_request(&body, &REMOTE);
    assert!(matches!(outcome.body, EgressBody::Unchanged));
    let epoch = outcome.policy_epoch();
    assert!(!epoch.is_current(None), "live: on; decided under: off");
    crate::test_env::set_var("LEAN_CTX_CONTEXT_GATEWAY", "off");
    assert!(epoch.is_current(None), "the state it was decided under");
    drop(off);

    // Between decision and `is_current`: the explicit re-validation reads the
    // live state and sees the change.
    let epoch = admit_request(&body, &REMOTE).policy_epoch();
    let _off = KillSwitchOff::set();
    assert!(!epoch.is_current(None));
}

/// A caller that refuses after admission gets a receipt with the deny decision,
/// and reads reason codes instead of parsing the refusal text.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_refusal_after_admission_is_recorded_as_a_deny_decision() {
    let _data = isolated("");
    let body = user("routine status update");
    let bytes = serde_json::to_vec(&body).expect("bytes");
    let admitted = admit_request(&body, &REMOTE);
    assert!(admitted.refusal_codes().is_empty(), "not refused yet");

    let refused = admitted.refuse(&[code("egress.policy_changed")]);
    let EgressBody::Refused(text) = &refused.body else {
        panic!("refused outcome expected: {:?}", refused.body);
    };
    assert!(text.contains("egress.policy_changed"), "{text}");
    assert_eq!(refused.refusal_codes(), [code("egress.policy_changed")]);

    let evidence = egress::finish_admission(&refused, Some(&bytes), bytes.len(), None)
        .expect("receipt")
        .expect("evidence");
    let receipt = evidence.decision_receipt.expect("decision receipt");
    assert!(receipt.final_context.is_none(), "nothing was prepared");
    assert!(receipt.decisions.iter().any(|decision| {
        decision.disposition == ContextDispositionV1::Deny
            && decision
                .reason_codes
                .iter()
                .any(|reason| reason.as_str() == "egress.policy_changed")
    }));
    assert_eq!(
        evidence.admission.expect("lifecycle admission").outcome,
        EgressAdmissionOutcomeV1::Withheld
    );
}

fn inert_detector(_: &str) -> crate::core::intelligence_runtime::Result<SemanticReport> {
    Ok(SemanticReport {
        bytes_inspected: 0,
        findings: Vec::new(),
    })
}

struct KillSwitchOff;

impl KillSwitchOff {
    fn set() -> Self {
        crate::test_env::set_var("LEAN_CTX_CONTEXT_GATEWAY", "off");
        Self
    }
}

impl Drop for KillSwitchOff {
    fn drop(&mut self) {
        crate::test_env::remove_var("LEAN_CTX_CONTEXT_GATEWAY");
    }
}

/// The epoch binds every effective input. Each change below leaves the
/// configuration bytes untouched.
#[test]
#[serial_test::serial(cache_telemetry)]
fn policy_epoch_changes_with_every_effective_input() {
    let _data = isolated("");
    let epoch = admit_request(&user("routine status update"), &REMOTE).policy_epoch();
    assert!(epoch.is_current(None));

    // The environment switch (developer mode honours it).
    {
        let _off = KillSwitchOff::set();
        assert!(!epoch.is_current(None), "environment switch flipped");
    }
    assert!(epoch.is_current(None), "and back");

    // Presence of the semantic detectors.
    let mut with_detectors = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    assert!(epoch.matches(&with_detectors, None));
    with_detectors.semantic =
        Some(crate::core::context_admission::semantic::SemanticDetector::fixed(inert_detector));
    assert!(!epoch.matches(&with_detectors, None), "detector presence");

    // Project, root and role travel in the caller's fingerprint — for opaque
    // outcomes as well.
    let opaque = admit_opaque_for_project_with_binding(
        64,
        &REMOTE,
        b"project-a",
        TrustedDestinationBinding::default(),
    )
    .policy_epoch();
    assert!(opaque.is_current(Some(b"project-a")));
    assert!(!opaque.is_current(Some(b"project-b")));
    assert!(
        !opaque.is_current(None),
        "a project-bound epoch is not a global one"
    );
    assert!(!epoch.is_current(Some(b"project-a")));
}

thread_local! {
    static RUNTIME_ROOTS_SEEN: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Q5 through the normal path: the policy is resolved by
/// `AdmissionPolicy::from_config` (permission check included), a request is
/// admitted, and its epoch ends when the configuration names ANOTHER runtime
/// while a detector stays present and no gateway or secret setting changes.
/// The same runtime again is the same epoch.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_runtime_change_that_keeps_a_detector_ends_the_policy_epoch() {
    use crate::core::intelligence_runtime::semantic_detectors::test_seam;
    let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let root = |directory: &tempfile::TempDir| {
        std::fs::canonicalize(directory.path())
            .expect("canonical runtime root")
            .to_str()
            .expect("utf-8 runtime root")
            .to_owned()
    };
    let (root_a, root_b) = (root(&first), root(&second));
    // A staging runtime in an existing directory is what the permission check
    // accepts without a bundled production channel.
    let settings = |root: &str| {
        format!(
            "[intelligence_runtime]\nenabled = true\naccept_proprietary = true\n\
             staging = true\nroot = '{root}'\n"
        )
    };
    let write = |root: &str| {
        let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
        std::fs::write(dir.join("config.toml"), settings(root)).expect("config");
    };
    let _data = isolated(&settings(&root_a));
    let _runtime = test_seam::stand_in(|runtime, text| {
        RUNTIME_ROOTS_SEEN.with(|seen| seen.borrow_mut().push(runtime.root.clone()));
        Ok(test_seam::clean_answer(text))
    });

    let resolved = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    assert!(
        resolved.semantic.is_some(),
        "the permitted runtime is bound through the normal path"
    );
    let epoch = admit_request(&user("routine status update"), &REMOTE).policy_epoch();
    assert!(epoch.is_current(None));
    let seen = RUNTIME_ROOTS_SEEN.with(|seen| seen.borrow().clone());
    assert!(!seen.is_empty(), "the request was inspected semantically");
    assert!(seen.iter().all(|seen| *seen == root_a), "{seen:?}");

    write(&root_b);
    let moved = AdmissionPolicy::from_config(&crate::core::config::Config::load_arc());
    assert!(moved.semantic.is_some(), "a detector is still present");
    assert!(
        !epoch.is_current(None),
        "another runtime is another policy epoch"
    );
    assert!(!epoch.matches(&moved, None));

    write(&root_a);
    assert!(epoch.is_current(None), "the same runtime again");
}

/// A receipt as JSON without the two fields that come from the capture's
/// clock (the id is seeded with the start instant, the duration measures it).
fn without_clock(
    receipt: &lean_ctx_protocol::context_gateway::ContextDecisionReceiptV1,
) -> serde_json::Value {
    let mut value = serde_json::to_value(receipt).expect("a receipt serializes");
    let fields = value.as_object_mut().expect("a receipt is an object");
    fields.remove("receipt_id");
    fields.remove("duration_us");
    value
}

/// A note states something about the request in its receipt and is not a
/// source: two notes with two codes are itemized as two allow decisions on the
/// fixed note object, and `sources` — inspected, permitted, selected, blocked
/// — is what the same request has without them. No note: the receipt is the
/// one the request has today. No capture: nothing is recorded.
#[test]
#[serial_test::serial(cache_telemetry)]
fn notes_are_itemized_in_the_receipt_and_never_count_as_sources() {
    let _data = isolated("");
    let body = json!({"model": "m", "messages": [
        {"role": "user", "content": "routine status update"},
        {"role": "user", "content": "second routine status update"},
    ]});
    let bytes = serde_json::to_vec(&body).expect("bytes");
    let receipt_of = |outcome: &egress::EgressOutcome| {
        egress::finish_admission(outcome, Some(&bytes), bytes.len(), None)
            .expect("receipt")
            .expect("evidence")
            .decision_receipt
            .expect("decision receipt")
    };
    let plain = receipt_of(&admit_request(&body, &LOCAL));
    assert!(
        plain
            .decisions
            .iter()
            .all(|decision| !super::capture::is_note(decision))
    );
    assert!(plain.sources.inspected > 0, "the request has sources");

    let unnoted = receipt_of(&admit_request(&body, &LOCAL).note_with(&[]));
    assert_eq!(
        without_clock(&unnoted),
        without_clock(&plain),
        "nothing to note: the receipt of today"
    );

    let noted = receipt_of(
        &admit_request(&body, &LOCAL)
            .note_with(&["recovery.unknown_lineage", "recovery.no_project"]),
    );
    let (notes, sources): (Vec<_>, Vec<_>) = noted
        .decisions
        .iter()
        .partition(|decision| super::capture::is_note(decision));
    assert_eq!(notes.len(), 2, "one decision per note");
    assert!(notes.iter().all(|decision| {
        decision.disposition == ContextDispositionV1::Allow
            && decision.reason_codes.len() == 1
            && decision.signals.is_empty()
            && decision.required_transformations.is_empty()
    }));
    let codes: Vec<&str> = notes
        .iter()
        .map(|decision| decision.reason_codes[0].as_str())
        .collect();
    assert_eq!(codes, ["recovery.unknown_lineage", "recovery.no_project"]);
    assert_eq!(
        sources,
        plain.decisions.iter().collect::<Vec<_>>(),
        "the decisions about sources are untouched"
    );
    assert_eq!(noted.sources, plain.sources, "a note is not a source");
    assert_eq!(noted.security, plain.security);
    assert_eq!(noted.outcome, plain.outcome);
    assert_eq!(noted.final_context, plain.final_context);
    assert_eq!(noted.tokens, plain.tokens);

    // A note changes neither the body nor whether a receipt is required.
    let outcome = admit_request(&body, &LOCAL).note_with(&["recovery.no_project"]);
    assert!(matches!(outcome.body, EgressBody::Unchanged));
    assert!(outcome.refusal_codes().is_empty());

    // Without a capture (the gateway is switched off) there is nothing to
    // write a note into, and no receipt appears because of one.
    let _off = KillSwitchOff::set();
    let passthrough = admit_request(&body, &LOCAL).note_with(&["recovery.no_project"]);
    assert!(matches!(passthrough.body, EgressBody::Unchanged));
    assert!(
        egress::finish_admission(&passthrough, Some(&bytes), bytes.len(), None)
            .expect("no receipt is required")
            .is_none()
    );
}

/// A receipt itemizes at most `MAX_DECISIONS` decisions. A source decision
/// that finds no slot is still counted; a note adds to no count, so a note
/// without a slot would be gone silently. Instead the receipt says so: one
/// fixed note stands for every note that is not itemized — in the slot of an
/// itemized note where there is one, else in the slot of a source decision,
/// which is then counted like every decision that is not itemized. The
/// source counts are the same with and without the notes.
#[test]
fn a_full_receipt_says_that_notes_are_not_itemized() {
    use lean_ctx_protocol::Sha256Digest;
    use lean_ctx_protocol::context_gateway::{
        ContextDecisionReceiptV1, ContextDecisionV1, GatewayModeV1, MAX_DECISIONS,
    };

    use super::AdmissionCounts;
    use super::capture::{self, AdmissionCapture, CallIdentity, Delivered, NOTES_NOT_ITEMIZED};
    // The receipt keys its objects under this data directory's receipt key.
    let _data = crate::core::data_dir::isolated_data_dir();

    let digest =
        |index: usize| Sha256Digest::new(format!("sha256:{index:064x}")).expect("canonical digest");
    let decision = |object: Sha256Digest, codes: &[&str]| ContextDecisionV1 {
        object,
        disposition: ContextDispositionV1::Allow,
        reason_codes: codes
            .iter()
            .map(|code| ReasonCodeV1::new(*code).expect("a valid reason code"))
            .collect(),
        signals: Vec::new(),
        required_transformations: Vec::new(),
    };
    // A request with `flagged` notable and `clean` clean source decisions,
    // followed by `notes` notes.
    let receipt_of = |flagged: usize, clean: usize, notes: usize| {
        let call = AdmissionCapture::new();
        call.bind_policy(GatewayModeV1::Governed, digest(usize::MAX));
        for index in 0..flagged {
            call.record(
                &decision(digest(index), &["injection.flagged"]),
                AdmissionCounts::default(),
            );
        }
        for index in 0..clean {
            call.record(
                &decision(digest(flagged + index), &[]),
                AdmissionCounts::default(),
            );
        }
        for _ in 0..notes {
            call.record(
                &decision(capture::note_object(), &["recovery.unknown_lineage"]),
                AdmissionCounts::default(),
            );
        }
        capture::finish(
            &call,
            &CallIdentity {
                agent_id: None,
                destination: None,
            },
            &Delivered {
                text: "delivered",
                is_error: false,
                tokens: 1,
            },
        )
        .expect("a valid receipt")
    };
    let notes_of = |receipt: &ContextDecisionReceiptV1| -> Vec<String> {
        receipt
            .decisions
            .iter()
            .filter(|decision| capture::is_note(decision))
            .map(|decision| decision.reason_codes[0].as_str().to_owned())
            .collect()
    };
    let full = u32::try_from(MAX_DECISIONS).expect("the bound fits");

    // Everything fits: every note is itemized, none stands for another.
    let fits = receipt_of(2, 1, 2);
    assert_eq!(
        notes_of(&fits),
        ["recovery.unknown_lineage", "recovery.unknown_lineage"]
    );
    assert_eq!(fits.sources, receipt_of(2, 1, 0).sources);
    assert_eq!(fits.sources.inspected, 3);

    // A clean source decision gives up its slot to a note and stays counted.
    let displaced = receipt_of(MAX_DECISIONS - 1, 1, 1);
    assert_eq!(displaced.decisions.len(), MAX_DECISIONS);
    assert_eq!(notes_of(&displaced), ["recovery.unknown_lineage"]);
    assert_eq!(
        displaced.sources,
        receipt_of(MAX_DECISIONS - 1, 1, 0).sources
    );
    assert_eq!(displaced.sources.inspected, full);

    // No clean slot, but itemized notes: the third note is not itemized, and
    // the last itemized one becomes the note that says so.
    let partly = receipt_of(MAX_DECISIONS - 2, 0, 3);
    assert_eq!(partly.decisions.len(), MAX_DECISIONS);
    assert_eq!(
        notes_of(&partly),
        ["recovery.unknown_lineage", NOTES_NOT_ITEMIZED]
    );
    assert_eq!(partly.sources, receipt_of(MAX_DECISIONS - 2, 0, 0).sources);

    // Every slot holds a notable source decision: no note is itemized, the
    // receipt says so once, and the source that gave up its slot is counted.
    let none = receipt_of(MAX_DECISIONS, 0, 3);
    assert_eq!(none.decisions.len(), MAX_DECISIONS);
    assert_eq!(notes_of(&none), [NOTES_NOT_ITEMIZED]);
    let unnoted = receipt_of(MAX_DECISIONS, 0, 0);
    assert_eq!(
        none.sources, unnoted.sources,
        "a note moves no source count"
    );
    assert_eq!(none.sources.inspected, full);
    assert_eq!(none.sources.permitted, full);
    assert_eq!(none.security, unnoted.security);
    assert_eq!(none.outcome, unnoted.outcome);
}
