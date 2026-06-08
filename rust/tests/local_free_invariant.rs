//! CI conformance gate for the **Local-Free Invariant** (RFC §6, EPIC 12.19):
//! the Personal (local) plane is never gated behind an account, license, or
//! plan. Commercial value is *additive* over a process/service boundary
//! (team/cloud), so this test fails the build if any local capability is ever
//! placed behind a paywall.

use lean_ctx::core::billing::{entitlement_allows, Plan};
use lean_ctx::core::server_capabilities::{
    capabilities_value, COMMERCIAL_PLANE_FEATURES, LOCAL_ALWAYS_ON_FEATURES,
    LOCAL_OPTIONAL_FEATURES,
};

#[test]
fn local_plane_is_default_and_free() {
    let v = capabilities_value();
    assert_eq!(
        v["plane"], "personal",
        "the default plane must be personal (local)"
    );
    for key in LOCAL_ALWAYS_ON_FEATURES {
        assert_eq!(
            v["features"][key],
            serde_json::json!(true),
            "local capability '{key}' must be free and always on"
        );
    }
}

#[test]
fn local_and_commercial_planes_are_disjoint() {
    for commercial in COMMERCIAL_PLANE_FEATURES {
        assert!(
            !LOCAL_ALWAYS_ON_FEATURES.contains(commercial),
            "'{commercial}' is commercial and must not be a local capability"
        );
        assert!(
            !LOCAL_OPTIONAL_FEATURES.contains(commercial),
            "'{commercial}' is commercial and must not be a local capability"
        );
    }
}

#[test]
fn local_features_are_unaffected_by_license_or_plan_env() {
    let snapshot = || {
        let v = capabilities_value();
        LOCAL_ALWAYS_ON_FEATURES
            .iter()
            .chain(LOCAL_OPTIONAL_FEATURES)
            .map(|k| (k.to_string(), v["features"][k].clone()))
            .collect::<Vec<_>>()
    };

    let before = snapshot();
    for var in ["LEAN_CTX_LICENSE", "LEAN_CTX_PLAN", "LEAN_CTX_ACCOUNT"] {
        std::env::set_var(var, "expired");
    }
    let after = snapshot();
    for var in ["LEAN_CTX_LICENSE", "LEAN_CTX_PLAN", "LEAN_CTX_ACCOUNT"] {
        std::env::remove_var(var);
    }

    assert_eq!(
        before, after,
        "no local capability may change based on a license/plan/account env var"
    );
}

#[test]
fn billing_plane_never_gates_a_local_feature() {
    // EPIC 13.6: the commercial billing layer must allow every local capability
    // on every plan — including Free. The local plane has no entitlement checks.
    for plan in Plan::all() {
        for feature in LOCAL_ALWAYS_ON_FEATURES
            .iter()
            .chain(LOCAL_OPTIONAL_FEATURES)
        {
            assert!(
                entitlement_allows(*plan, feature),
                "billing gated local feature '{feature}' on plan {plan:?}"
            );
        }
    }
}
