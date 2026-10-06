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
