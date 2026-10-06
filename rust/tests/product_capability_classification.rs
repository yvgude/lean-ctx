// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;

use lean_ctx::core::billing::{Plan, entitlement_allows};
use lean_ctx::core::ocla::OclaCapabilityKind;
use lean_ctx::core::product_capabilities::{
    EXPERIMENT_EXECUTOR_CAPABILITY_ID, ProductCapabilityRegistry, REGISTRY_SOURCE,
    ocla_product_capability_id, registry,
};
use lean_ctx::core::server_capabilities::{
    COMMUNITY_ALWAYS_ON_FEATURES, COMMUNITY_OPTIONAL_FEATURES, capabilities_value,
};

const REQUIRED_ORCHESTRATION_IDS: [&str; 20] = [
    "trust.ocla_event_bus",
    "trust.a2a_contracts",
    "trust.manual_handoff",
    "pro.agent_bus.local",
    "pro.agent_presence.local",
    "pro.agent_handoff.automatic",
    "pro.agent_knowledge.local",
    "pro.agent_leases.local",
    "pro.work_graph.local",
    "pro.result_fusion",
    "pro.execution_attribution",
    "team.agent_bus.workspace",
    "team.work_graph.shared",
    "team.agent_presence.shared",
    "team.context.shared",
    "team.leases.shared",
    "enterprise.agent_identity.governed",
    "enterprise.agent_attestation",
    "enterprise.agent_bus.governed",
    "enterprise.execution_policy",
];

#[test]
fn registry_is_valid_unique_and_covers_required_orchestration() {
    let repository_source = include_str!("../../product/capabilities.toml");
    assert_eq!(
        REGISTRY_SOURCE, repository_source,
        "package-local capability registry mirror drifted from product SSOT"
    );
    let parsed = ProductCapabilityRegistry::parse(REGISTRY_SOURCE).expect("valid registry");
    let ids: BTreeSet<&str> = parsed
        .entries()
        .iter()
        .map(|capability| capability.id.as_str())
        .collect();
    assert_eq!(ids.len(), parsed.entries().len());
    for id in REQUIRED_ORCHESTRATION_IDS {
        assert!(ids.contains(id), "missing required capability '{id}'");
    }
}

#[test]
fn every_ocla_builtin_and_experiment_executor_is_classified() {
    for kind in OclaCapabilityKind::ALL {
        let id = ocla_product_capability_id(kind);
        assert!(registry().find(id).is_some(), "{kind:?} missing '{id}'");
    }
    assert!(registry().find(EXPERIMENT_EXECUTOR_CAPABILITY_ID).is_some());
}

#[test]
fn every_server_feature_is_classified_exactly_once() {
    let features = capabilities_value();
    let features = features["features"].as_object().expect("features object");
    for key in features.keys() {
        assert!(
            registry().find(key).is_some(),
            "unclassified feature '{key}'"
        );
    }
    assert_eq!(
        features.len(),
        COMMUNITY_ALWAYS_ON_FEATURES.len() + COMMUNITY_OPTIONAL_FEATURES.len(),
        "every advertised server primitive must remain explicitly classified"
    );
    assert_eq!(
        registry()
            .find("routing")
            .expect("routing classified")
            .minimum_plan(),
        Plan::Community
    );
    assert_eq!(
        registry()
            .find("pro.runtime.adaptive_routing")
            .expect("adaptive routing classified")
            .minimum_plan(),
        Plan::Community
    );
}

#[test]
fn decisions_fail_closed_and_plans_are_monotonic() {
    for capability in registry().entries() {
        let decisions: Vec<bool> = Plan::all()
            .iter()
            .map(|plan| entitlement_allows(*plan, &capability.id))
            .collect();
        assert!(
            decisions.windows(2).all(|pair| !pair[0] || pair[1]),
            "non-monotonic decisions for '{}': {decisions:?}",
            capability.id
        );
    }
    for plan in Plan::all() {
        assert!(!entitlement_allows(*plan, "unknown.capability"));
    }
}

#[test]
fn community_fallback_remains_accountless_and_useful() {
    assert_eq!(capabilities_value()["plane"], "personal");
    for key in COMMUNITY_ALWAYS_ON_FEATURES
        .iter()
        .chain(COMMUNITY_OPTIONAL_FEATURES)
    {
        let capability = registry().find(key).expect("Community feature classified");
        assert_eq!(capability.minimum_plan(), Plan::Community);
        assert!(!capability.account_required);
        assert!(entitlement_allows(Plan::Community, key));
    }
    assert!(entitlement_allows(Plan::Community, "pro.agent_bus.local"));
}

#[test]
fn community_availability_is_independent_of_account_environment() {
    let _env_lock = lean_ctx::core::data_dir::test_env_lock();
    let snapshot = || {
        let value = capabilities_value();
        COMMUNITY_ALWAYS_ON_FEATURES
            .iter()
            .chain(COMMUNITY_OPTIONAL_FEATURES)
            .map(|key| (key.to_string(), value["features"][key].clone()))
            .collect::<Vec<_>>()
    };
    let before = snapshot();
    for variable in ["LEAN_CTX_LICENSE", "LEAN_CTX_PLAN", "LEAN_CTX_ACCOUNT"] {
        // SAFETY: the test environment lock serializes environment mutation.
        unsafe { std::env::set_var(variable, "expired") };
    }
    let after = snapshot();
    for variable in ["LEAN_CTX_LICENSE", "LEAN_CTX_PLAN", "LEAN_CTX_ACCOUNT"] {
        // SAFETY: the test environment lock remains held.
        unsafe { std::env::remove_var(variable) };
    }
    assert_eq!(before, after);
}

#[test]
fn generated_product_matrix_is_current() {
    let committed = include_str!("../../docs/reference/generated/product-capabilities.md")
        .replace("\r\n", "\n");
    assert_eq!(registry().render_markdown(), committed);
}
