// SPDX-License-Identifier: Apache-2.0

//! CI gate for the Local-Free Invariant v2
//! (`docs/contracts/local-free-invariant-v2.md`): everything the Engine does on
//! one machine works without an account, licence or plan. Paid rights are
//! LeanCTX Hive plans and only ever add capabilities. This test fails the
//! build if the default plane is not personal, an always-on Community feature
//! is off, a local capability reacts to a licence, plan or account variable or
//! to a forged entitlement cache, or a free capability needs an account.

use std::collections::BTreeMap;

use lean_ctx::core::product_capabilities::{Plan, UserPriceClass, registry};
use lean_ctx::core::server_capabilities::{
    COMMUNITY_ALWAYS_ON_FEATURES, COMMUNITY_OPTIONAL_FEATURES, capabilities_value,
};
use serde_json::{Value, json};

fn entitlement_allows(plan: Plan, id: &str) -> bool {
    registry().allows(plan, id)
}

const GATING_VARIABLES: [&str; 3] = ["LEAN_CTX_LICENSE", "LEAN_CTX_PLAN", "LEAN_CTX_ACCOUNT"];

/// Everything a local decision could depend on: the advertised Community
/// features and whether each free capability is granted to Community.
fn local_snapshot() -> (Vec<(String, Value)>, BTreeMap<String, bool>) {
    let value = capabilities_value();
    let features = COMMUNITY_ALWAYS_ON_FEATURES
        .iter()
        .chain(COMMUNITY_OPTIONAL_FEATURES)
        .map(|key| ((*key).to_string(), value["features"][key].clone()))
        .collect();
    let free = registry()
        .entries()
        .iter()
        .filter(|entry| entry.price_class == UserPriceClass::Free)
        .map(|entry| {
            (
                entry.id.clone(),
                entitlement_allows(Plan::Community, &entry.id),
            )
        })
        .collect();
    (features, free)
}

#[test]
fn local_plane_is_default_and_free() {
    let value = capabilities_value();
    assert_eq!(
        value["plane"], "personal",
        "the default plane must be personal"
    );
    for key in COMMUNITY_ALWAYS_ON_FEATURES {
        assert_eq!(
            value["features"][key],
            json!(true),
            "local capability '{key}' must be free and always on"
        );
    }
}

#[test]
fn every_free_capability_is_accountless_and_granted_to_community() {
    for entry in registry().entries() {
        if entry.price_class == UserPriceClass::Free {
            assert!(
                !entry.account_required,
                "{}: a free capability must not require an account",
                entry.id
            );
            assert_eq!(entry.minimum_plan(), Plan::Community, "{}", entry.id);
            assert!(
                entitlement_allows(Plan::Community, &entry.id),
                "{}: free but refused to Community",
                entry.id
            );
        }
    }
    // Every advertised local feature is classified free.
    for key in COMMUNITY_ALWAYS_ON_FEATURES
        .iter()
        .chain(COMMUNITY_OPTIONAL_FEATURES)
    {
        let entry = registry().find(key).expect("local feature classified");
        assert_eq!(entry.price_class, UserPriceClass::Free, "{key}");
    }
}

#[test]
fn local_capabilities_ignore_licence_plan_account_and_a_forged_entitlement_cache() {
    let _env_lock = lean_ctx::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().expect("temp data dir");
    let previous_data = std::env::var_os("LEAN_CTX_DATA_DIR");
    // SAFETY: the test environment lock serializes environment mutation.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data.path()) };
    let before = local_snapshot();

    for variable in GATING_VARIABLES {
        // SAFETY: the test environment lock remains held.
        unsafe { std::env::set_var(variable, "expired") };
    }
    // A forged, unsigned entitlement cache claiming a paid plan must not
    // change any local decision.
    for name in ["entitlement.json", "plan.json", "license.json"] {
        std::fs::write(
            data.path().join(name),
            r#"{"plan":"enterprise","capabilities":["pro.runtime.code_security"],"seats":1}"#,
        )
        .expect("write forged cache");
    }
    let after = local_snapshot();
    for variable in GATING_VARIABLES {
        // SAFETY: the test environment lock remains held.
        unsafe { std::env::set_var(variable, "enterprise") };
    }
    let upgraded = local_snapshot();

    for variable in GATING_VARIABLES {
        // SAFETY: the test environment lock remains held.
        unsafe { std::env::remove_var(variable) };
    }
    match previous_data {
        // SAFETY: the test environment lock remains held.
        Some(value) => unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", value) },
        // SAFETY: the test environment lock remains held.
        None => unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") },
    }

    assert_eq!(
        before, after,
        "no local capability may change on a licence/plan/account variable or a forged cache"
    );
    assert_eq!(before, upgraded, "a claimed paid plan adds nothing locally");
    assert!(before.1.values().all(|granted| *granted));
}

#[test]
fn paid_rights_are_hive_plans_and_never_free_capabilities() {
    for entry in registry().entries() {
        match entry.price_class {
            UserPriceClass::Hive => {
                assert!(entry.hive_plan.is_some(), "{}", entry.id);
                assert!(entry.account_required, "{}", entry.id);
                assert!(
                    !entitlement_allows(Plan::Community, &entry.id),
                    "{}",
                    entry.id
                );
            }
            UserPriceClass::Retired => {
                for plan in Plan::all() {
                    assert!(!entitlement_allows(*plan, &entry.id), "{}", entry.id);
                }
            }
            UserPriceClass::Free | UserPriceClass::SdkOemCommercial => {}
            other => panic!("{}: legacy price class {other:?} in schema 3", entry.id),
        }
    }
}
