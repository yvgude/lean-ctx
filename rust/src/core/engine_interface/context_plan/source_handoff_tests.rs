// SPDX-License-Identifier: Apache-2.0

use super::*;
use lean_ctx_protocol::EngineContextSourcePlanRequestV1;
use serde_json::json;

#[test]
fn materialized_handoff_retains_the_exact_lineage_projection() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let content = "invoice ledger duplicate handoff";
    let digest = super::super::sha256_digest(content.as_bytes()).unwrap();
    let sources = (1..=2)
        .map(|index| {
            json!({
                "descriptor": {"object_ref": format!("source:handoff-{index}"),
                    "source_id": "handoff-provider", "source_type": "other",
                    "content_digest": digest, "revision": "r1", "owner": "operator",
                    "observed_at": null, "valid_until": null, "classification": "Public",
                    "permission": "permitted"}, "content": content,
            })
        })
        .collect::<Vec<_>>();
    let request: EngineContextSourcePlanRequestV1 = serde_json::from_value(json!({
        "planning": {"schema_version": 1, "transport_version": 1,
            "engine_interface_version": "1.0.0", "task_id": "handoff-task",
            "query": "invoice ledger", "budget_tokens": 512, "max_candidates": 64},
        "sources": sources,
    }))
    .unwrap();
    let plan = sources::plan(&root, request.clone()).unwrap();
    let (snapshot, decision) = materialize_sources_with_decision(
        &root,
        &EngineContextSourceMaterializationRequestV1 {
            source_plan: request,
            expected_binding_digest: plan.binding_digest.clone(),
            planning_evaluation_time: None,
        },
    )
    .unwrap();
    assert_eq!(snapshot.plan, plan);
    assert_eq!(decision.context_projection(), &snapshot.plan.result.plan);
    let projection = decision.context_projection();
    assert_eq!(
        projection.projection_digest.as_ref(),
        Some(&projection.compute_projection_digest().unwrap())
    );
    let lineage = projection.extensions.get("source_lineage_v1").unwrap();
    assert_eq!(
        lineage["groups"][0]["equivalent_sources"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let canonical: serde_json::Value =
        serde_json::from_slice(&decision.canonical_bytes().unwrap()).unwrap();
    assert_eq!(canonical[0], serde_json::to_value(projection).unwrap());
}

fn presented_request(content: &str, classification: &str) -> EngineContextSourcePlanRequestV1 {
    let digest = super::super::sha256_digest(content.as_bytes()).unwrap();
    serde_json::from_value(json!({
        "planning": {"schema_version": 1, "transport_version": 1,
            "engine_interface_version": "1.0.0", "task_id": "presented-task",
            "query": "invoice ledger", "budget_tokens": 512, "max_candidates": 64},
        "sources": [{
            "descriptor": {"object_ref": "source:presented-1",
                "source_id": "presented-provider", "source_type": "other",
                "content_digest": digest, "revision": "r1", "owner": "operator",
                "observed_at": null, "valid_until": null, "classification": classification,
                "permission": "permitted"}, "content": content,
        }],
    }))
    .unwrap()
}

fn materialized(
    root: &std::path::Path,
    request: &EngineContextSourcePlanRequestV1,
) -> Result<String, &'static str> {
    let plan = sources::plan(root, request.clone())?;
    materialize(
        root,
        &EngineContextSourceMaterializationRequestV1 {
            source_plan: request.clone(),
            expected_binding_digest: plan.binding_digest,
            planning_evaluation_time: None,
        },
    )
    .map(|response| response.content)
}

fn lineage_state(
    root: &std::path::Path,
    text: &str,
) -> Option<Result<(), crate::core::archive::delivered::LineageRefusal>> {
    use crate::core::archive::delivered;
    crate::core::policy::runtime::REQUEST_PROJECT
        .sync_scope(std::cell::RefCell::new(Some(root.to_path_buf())), || {
            delivered::lineage_of(&delivered::digest(text)).map(|lineage| lineage.still_admitted())
        })
}

/// The real plan → materialize path for a caller-supplied source. The
/// materialized text is registered with a PRESENTED lineage (never a file
/// authority): it holds while the caller's presentation is on record under
/// the current policy, and is refused with a distinct reason when the
/// presentation is missing or the policy has changed. Before, prepared
/// context left the Engine with no lineage at all.
#[test]
fn materialized_context_is_registered_with_a_presented_lineage() {
    use crate::core::archive::delivered::LineageRefusal;
    let data = crate::core::data_dir::isolated_data_dir();
    let community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let request = presented_request("invoice ledger for customer K-482193", "Public");

    let text = materialized(&root, &request).unwrap();
    assert!(text.contains("K-482193"));
    assert_eq!(lineage_state(&root, &text), Some(Ok(())));
    // Exact bytes only.
    assert_eq!(lineage_state(&root, text.trim_end()), None);

    // No presentation on record (what an elapsed validity leaves behind):
    // distinct from a refusal under current rights.
    let presented = data
        .path()
        .join("archives")
        .join(crate::core::archive::presented::REGISTER_DIRECTORY);
    std::fs::remove_dir_all(&presented).expect("presentation register");
    assert_eq!(
        lineage_state(&root, &text),
        Some(Err(LineageRefusal::SourceNotPresented))
    );
    // Presented again through the same path.
    assert_eq!(materialized(&root, &request).unwrap(), text);
    assert_eq!(lineage_state(&root, &text), Some(Ok(())));

    // Another project's request does not see this project's lineage.
    let elsewhere = tempfile::tempdir().unwrap();
    let other_root = std::fs::canonicalize(elsewhere.path()).unwrap();
    assert_eq!(lineage_state(&other_root, &text), None);

    // The project policy is tightened (the customer number is now masked).
    // The text delivered under the old policy is refused — also after the
    // caller presents the source again; what the current policy delivers is
    // a different text with its own lineage.
    //
    // The policy identity of a presentation is read for the explicit project
    // (`policy::runtime::for_project`: the project's own pack, never the
    // process-wide test override). So the policy is tightened where the
    // product reads it — the pack file of this project — and the override,
    // which would pin `runtime::active()` to "no pack", ends first; its lock
    // is kept so that no other test fixes a policy meanwhile.
    let tightened = "name = \"tightened\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
                     [redaction]\ncustomer = 'K-[0-9]{6}'\n";
    crate::core::policy::parse(tightened).expect("valid policy");
    drop(community);
    let _live_policy = crate::core::policy::runtime::lock_test_override();
    std::fs::create_dir_all(root.join(".lean-ctx")).expect("policy directory");
    std::fs::write(root.join(".lean-ctx").join("policy.toml"), tightened).expect("policy pack");
    assert!(
        crate::core::policy::runtime::for_project(&root)
            .expect("the pack loads")
            .is_some(),
        "the tightened pack is the project's policy now"
    );
    assert_eq!(
        lineage_state(&root, &text),
        Some(Err(LineageRefusal::SourceNoLongerAdmitted))
    );
    let again = materialized(&root, &request);
    assert_eq!(
        lineage_state(&root, &text),
        Some(Err(LineageRefusal::SourceNoLongerAdmitted)),
        "presented again, but this text is not what the current policy delivers"
    );
    if let Ok(current) = again {
        assert!(!current.contains("K-482193"), "{current}");
        assert_eq!(lineage_state(&root, &current), Some(Ok(())));
    }
}

/// The v2 path, plan → materialize under a presenter capability, and what a
/// later request may reuse. Every `lineage_state` below stands for another
/// process: it shares the store on disk with the presenting call and has
/// nothing of it in memory but the capability it shows (or none).
///
/// A capability reuses the presentations made under it, and nothing else:
/// not another presenter's source, nothing without being shown, nothing once
/// lapsed, revoked or expired — one refusal for all of these. Two presenters
/// of the same source both reuse it, and neither's lapse or revocation
/// touches the other. Before, the presenter was the OS user, so B could
/// reuse what only A's presentation justified.
#[test]
fn a_presenter_capability_reuses_its_own_presentations_and_nothing_else() {
    use super::presenter::CAPABILITY_REFUSED;
    use crate::core::archive::delivered::LineageRefusal;
    use crate::core::archive::presented::{self, PresenterCapability, TestClock};
    use lean_ctx_protocol::{
        EngineContextSourceMaterializationRequestV2, EnginePresenterRequestV1,
    };

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let shared = presented_request("invoice ledger for customer K-771045", "Public");
    let only_a = presented_request("invoice ledger for customer K-771046", "Public");
    let issue = EnginePresenterRequestV1::Issue { ttl_seconds: 3600 };
    let renew = EnginePresenterRequestV1::Renew;
    let present = |request: &EngineContextSourcePlanRequestV1,
                   mode: EnginePresenterRequestV1,
                   shown: Option<&PresenterCapability>| {
        let plan = sources::plan(&root, request.clone()).unwrap();
        presenter::materialize_v2(
            &root,
            &EngineContextSourceMaterializationRequestV2 {
                schema_version: 2,
                materialization: EngineContextSourceMaterializationRequestV1 {
                    source_plan: request.clone(),
                    expected_binding_digest: plan.binding_digest,
                    planning_evaluation_time: None,
                },
                presenter: mode,
            },
            shown.cloned(),
        )
    };
    let reuse = |shown: Option<&PresenterCapability>, text: &str| {
        presented::with_presenter(shown.cloned(), || lineage_state(&root, text))
    };
    let held = |wire: Option<&str>| PresenterCapability::parse(wire.expect("issued")).unwrap();
    let (ok, refused) = (Some(Ok(())), Some(Err(LineageRefusal::SourceNotPresented)));

    // A presents two sources under one capability; B presents one of them.
    let a_first = present(&shared, issue, None).unwrap();
    let a = held(a_first.presenter.capability.as_deref());
    let a_second = present(&only_a, renew, Some(&a)).unwrap();
    assert!(
        a_second.presenter.capability.is_none(),
        "a capability is handed out once"
    );
    assert_eq!(
        a_second.presenter.presenter_ref,
        a_first.presenter.presenter_ref
    );
    let b_first = present(&shared, issue, None).unwrap();
    let b = held(b_first.presenter.capability.as_deref());
    assert_ne!(
        b_first.presenter.presenter_ref,
        a_first.presenter.presenter_ref
    );
    let shared_text = a_first.materialization.content.clone();
    let a_text = a_second.materialization.content.clone();
    assert_eq!(b_first.materialization.content, shared_text);
    for response in [&a_first, &b_first] {
        let wire = response.presenter.capability.as_deref().expect("issued");
        assert!(!response.materialization.content.contains(wire));
        assert!(!format!("{response:?}").contains(wire));
    }
    let unknown = held(Some(format!("lcp1_{}", "0f".repeat(32)).as_str()));

    for (case, shown, text, expected) in [
        ("A, later process, first source", Some(&a), &shared_text, ok),
        ("A, later process, second source", Some(&a), &a_text, ok),
        ("B, the source both presented", Some(&b), &shared_text, ok),
        ("B, a source only A presented", Some(&b), &a_text, refused),
        ("no capability shown", None, &shared_text, refused),
        ("no capability shown, A's source", None, &a_text, refused),
        (
            "a capability nobody issued",
            Some(&unknown),
            &shared_text,
            refused,
        ),
    ] {
        assert_eq!(reuse(shown, text), expected, "{case}");
    }
    // The mode and the shown capability have to fit.
    assert_eq!(
        present(&shared, issue, Some(&a)).err(),
        Some(CAPABILITY_REFUSED)
    );
    assert_eq!(
        present(&shared, renew, None).err(),
        Some(CAPABILITY_REFUSED)
    );
    assert_eq!(
        present(&shared, renew, Some(&unknown)).err(),
        Some(CAPABILITY_REFUSED)
    );

    {
        // Ten minutes on, nobody has presented again: both have lapsed. A
        // presents again; B's lapse changes nothing for A, and A's
        // presentation revives nothing for B.
        let _later = TestClock::past_validity();
        assert_eq!(reuse(Some(&a), &shared_text), refused);
        present(&shared, renew, Some(&a)).unwrap();
        assert_eq!(reuse(Some(&a), &shared_text), ok);
        assert_eq!(reuse(Some(&b), &shared_text), refused, "B lapsed");
        // B presents again too, then revokes: B is out, A is untouched.
        present(&shared, renew, Some(&b)).unwrap();
        assert_eq!(reuse(Some(&b), &shared_text), ok);
        assert!(presented::revoke(&b));
        assert_eq!(reuse(Some(&b), &shared_text), refused, "revoked");
        assert_eq!(
            present(&shared, renew, Some(&b)).err(),
            Some(CAPABILITY_REFUSED)
        );
        assert_eq!(reuse(Some(&a), &shared_text), ok, "A outlives B");
    }

    // Past the capability's lifetime nothing is reused and nothing renewed.
    let _expired = TestClock::past_capability_lifetime();
    assert_eq!(reuse(Some(&a), &shared_text), refused, "expired");
    assert_eq!(
        present(&shared, renew, Some(&a)).err(),
        Some(CAPABILITY_REFUSED)
    );
}

/// A restricted caller-supplied source is in no reusable lineage (E3):
/// whether the plan delivers it or not, nothing is registered for it.
#[test]
fn a_restricted_presented_source_leaves_no_lineage() {
    let data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let request = presented_request("invoice ledger restricted figures", "Restricted");
    if let Ok(text) = materialized(&root, &request) {
        assert_eq!(lineage_state(&root, &text), None);
    }
    assert!(
        !data
            .path()
            .join("archives")
            .join(crate::core::archive::presented::REGISTER_DIRECTORY)
            .exists(),
        "no presentation is recorded for a restricted source"
    );
}
