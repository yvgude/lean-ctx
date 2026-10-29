// SPDX-License-Identifier: Apache-2.0
//! T-VETO host part (SPEC-ENGINE §6.2, V9.5): after a maintainer's veto of
//! a host lock or handoff the holder's next `ctx_tasks`/`ctx_agent` call is
//! refused `claim_vetoed`; a forged control is ignored and counted
//! `sig_invalid`.

use std::collections::BTreeMap;

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde_json::Value;

use super::*;
use crate::core::agent_lease::{
    AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRequestV1, AgentLeaseResourceKindV1,
};
use crate::core::hive::bus::handoff::{Handoffs, Offer, OfferState};
use crate::core::hive::bus::leases::{Binding, Bindings};
use crate::core::hive::bus::relay_v2::StateV2;
use crate::core::hive::bus::v2::{CONTROL_DOMAIN, ControlKeys};
use crate::core::hive::vault::{VaultMap, VaultRecord};

const VAULT: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
const WORKER: &str = "wkr_vk4oelpv5lzjmev44hun";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The Hive control key of the C0 vectors (purpose `control`).
fn control_key() -> (SigningKey, ControlKeys) {
    let keys = fixture("vectors/signing.vectors.json");
    let k = &keys["keys"]["hive_control"];
    let mut seed = [0u8; 32];
    hex::decode_to_slice(k["seed_hex"].as_str().unwrap(), &mut seed).unwrap();
    let signing = SigningKey::from_bytes(&seed);
    let mut public = [0u8; 32];
    hex::decode_to_slice(k["public_key_hex"].as_str().unwrap(), &mut public).unwrap();
    assert_eq!(signing.verifying_key().to_bytes(), public);
    let keys = BTreeMap::from([(
        k["key_id"].as_str().unwrap().to_string(),
        VerifyingKey::from_bytes(&public).unwrap(),
    )]);
    (signing, keys)
}

/// The veto fixture, re-pointed at `ref` and signed with the control key.
fn veto(kind: &str, id: &str) -> Control {
    let (signing, _) = control_key();
    let mut value = fixture("bus/control.veto.json");
    value["ref"] = serde_json::json!({"kind": kind, "id": id});
    value.as_object_mut().unwrap().remove("signature");
    let input =
        lean_ctx_protocol::hive::signing_input_without_signature(CONTROL_DOMAIN, &value).unwrap();
    value["signature"] = Value::String(hex::encode(signing.sign(&input).to_bytes()));
    serde_json::from_value(value).unwrap()
}

fn store() -> (tempfile::TempDir, HiveStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("config"), dir.path().join("data"));
    let mut map = VaultMap::default();
    map.vaults.insert(
        "stable".into(),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: "pay".into(),
            sealed: false,
            roots: vec![],
        },
    );
    map.save(store.data_dir()).unwrap();
    (dir, store)
}

/// A host lock: the local lease of `agent` and its Hive binding `bl_7`.
fn hold_lock(store: &HiveStore, agent: &str) -> String {
    let root = store.data_dir().to_string_lossy().into_owned();
    let resource_ref = crate::core::agent_lease::project_path_resource(&root, "src/a.rs").unwrap();
    let granted = crate::core::agent_lease::acquire_shared(AgentLeaseRequestV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: format!("lease-request:{agent}"),
        resource_kind: AgentLeaseResourceKindV1::Path,
        resource_ref,
        owner_agent_id: agent.into(),
        duration_ms: 600_000,
    })
    .unwrap();
    let AgentLeaseAcquireV1::Granted(lease) = granted else {
        panic!("lease granted")
    };
    Bindings::mutate(store.data_dir(), |b| {
        b.leases.insert(
            lease.lease_ref.clone(),
            Binding {
                lease_id: "bl_7".into(),
                fencing_token: 4,
                vault_id: VAULT.into(),
                expires_at: "2099-01-01T00:00:00Z".into(),
            },
        );
    })
    .unwrap();
    lease.lease_ref
}

#[test]
fn a_veto_of_a_host_lock_refuses_the_holders_next_call_once() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let (_dir, store) = store();
    let lease_ref = hold_lock(&store, "agent-a");
    let (_, keys) = control_key();
    let control = veto("lease", "bl_7");
    let mut state = StateV2::default();
    assert!(state.apply_control(false, control.clone(), &keys));
    state.save(store.data_dir()).unwrap();

    let worker_of = |agent: &str| (agent == "agent-a").then(|| WORKER.to_string());
    let applied = apply(&store, &control, &worker_of).unwrap();
    assert_eq!(applied.agents, ["agent-a"]);
    assert!(applied.progress_queued, "progress 'vetoed: …' is queued");
    // Dropped locally: the binding and the local lease behind it.
    assert!(Bindings::load(store.data_dir()).leases.is_empty());
    assert_eq!(
        crate::core::agent_lease::revoke_shared(&lease_ref).unwrap(),
        None
    );

    // The holder's next ctx_tasks / ctx_agent call: claim_vetoed, once.
    let refusal = take_refusal_in(&store, "agent-a").unwrap();
    assert!(refusal.starts_with("claim_vetoed: A maintainer vetoed this claim (lease bl_7)"));
    assert!(
        crate::core::hive::untrusted::every_occurrence_wrapped(
            &refusal,
            "The migration needs a maintainer"
        ),
        "the reason is fenced: {refusal}"
    );
    assert_eq!(take_refusal_in(&store, "agent-a"), None);
    assert_eq!(take_refusal_in(&store, "agent-b"), None);
    assert!(!answers("ctx_read") && answers("ctx_tasks") && answers("ctx_agent"));
    // The veto stands for later references to the lock (renew, re-acquire).
    assert!(vetoed_v2(&store, "bl_7").is_some());
    assert!(vetoed_v2(&store, "bl_8").is_none());
    // Applying it again (a replayed frame) adds no second notice.
    apply(&store, &control, &worker_of).unwrap();
    assert_eq!(take_refusal_in(&store, "agent-a"), None);
}

#[test]
fn a_vetoed_handoff_is_rejected_and_its_offerer_refused_once() {
    let (_dir, store) = store();
    Handoffs::mutate(store.data_dir(), |h| {
        h.offers.insert(
            "hof_1".into(),
            Offer {
                from: "agent-b".into(),
                to: "peer".into(),
                vault_id: VAULT.into(),
                org_id: "org".into(),
                entry_id: "msg_00000000000000a4".into(),
                deadline: "2099-01-01T00:00:00Z".into(),
                state: OfferState::Offered,
            },
        );
    })
    .unwrap();
    // The C0 fixture as Hive signed it.
    let control: Control = serde_json::from_value(fixture("bus/control.veto.json")).unwrap();
    let (_, keys) = control_key();
    assert!(StateV2::default().apply_control(false, control.clone(), &keys));
    let applied = apply(&store, &control, &|_| Some(WORKER.to_string())).unwrap();
    assert_eq!(applied.agents, ["agent-b"]);
    assert_eq!(
        Handoffs::load(store.data_dir()).offers["hof_1"].state,
        OfferState::Rejected {
            by: "maintainer".into(),
            reason: "veto".into()
        }
    );
    assert!(take_refusal_in(&store, "agent-b").is_some());
}

#[test]
fn a_forged_control_is_ignored_and_counted_sig_invalid() {
    let (_, keys) = control_key();
    let mut forged = veto("lease", "bl_7");
    forged.reason = "Forged by someone without the control key.".into();
    let mut state = StateV2::default();
    assert!(!state.apply_control(false, forged, &keys));
    assert_eq!(state.sig_invalid, 1);
    assert!(state.vetoed("bl_7").is_none(), "nothing is applied");
}
