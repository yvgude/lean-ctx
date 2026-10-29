// SPDX-License-Identifier: Apache-2.0
//! Receipts v2: local receipt → closed document, signing, gates, contract
//! fixtures (C0 2.5, copied byte for byte).

use super::*;
use crate::core::hive::vault::VaultRecord;
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::lease::{HiveOrgSettingsV1, HivePlan, LEASE_SCHEMA};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/lean-ctx-protocol/tests/fixtures/hive-v2/"
);

fn fixture_value(name: &str) -> Value {
    let text = std::fs::read_to_string(format!("{FIXTURES}{name}")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn lease(rights: Vec<HiveRight>, receipts: Option<bool>) -> HiveLeaseV1 {
    HiveLeaseV1 {
        schema: LEASE_SCHEMA.into(),
        lease_id: "lse_1".into(),
        org_id: "org_acme".into(),
        instance_id: "inst_1".into(),
        plan: HivePlan::Enterprise,
        seats_limit: 25,
        rights,
        policy_signer_keys: vec![],
        issued_at: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
        expires_at: UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
        grace_until: UtcTimestamp::new("2026-10-27T12:00:00Z").unwrap(),
        server_time: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
        org_settings: receipts.map(|receipts_upload| HiveOrgSettingsV1 {
            metrics_upload: true,
            receipts_upload,
        }),
    }
}

/// Content that must never reach Hive, inside the local receipt.
const SECRET_PATH: &str = "Users/ada/payments/src/main.rs";

fn local_receipt(failed: bool) -> (EngineInvocationV1, EngineObservationV1) {
    let input_digest = format!("sha256:{}", "ab".repeat(32));
    let invocation: EngineInvocationV1 = serde_json::from_value(json!({
        "schema_version": 1,
        "invocation_id": "engine-invocation-ada-payments",
        "engine": {"engine_id": "lean-ctx-local", "engine_version": "3.12.0"},
        "operation": {
            "capability_id": "capability://leanctx/context-optimization",
            "capability_version": "1.0.0"
        },
        "input_ref": format!("source:{SECRET_PATH}"),
        "input_digest": input_digest,
        "source_refs": [format!("source:{SECRET_PATH}"), "source:lineage-2"],
        "policy_admission": {"policy_ref": "policy:ctx-read-context-gate-v1:fixture", "decision": "admitted"}
    }))
    .unwrap();
    let observation: EngineObservationV1 = if failed {
        serde_json::from_value(json!({
            "schema_version": 1,
            "invocation_id": invocation.invocation_id.as_str(),
            "status": "failed",
            "source_lineage": [format!("source:{SECRET_PATH}")],
            "measurements": [],
            "failure": {
                "code": "source_unavailable",
                "retryable_by_host": false,
                "recovery_ref": format!("recovery:{SECRET_PATH}")
            }
        }))
        .unwrap()
    } else {
        serde_json::from_value(json!({
            "schema_version": 1,
            "invocation_id": invocation.invocation_id.as_str(),
            "status": "succeeded",
            "output_ref": format!("output:{}", "cd".repeat(32)),
            "output_digest": format!("sha256:{}", "cd".repeat(32)),
            "source_lineage": [format!("source:{SECRET_PATH}"), "source:lineage-2"],
            "measurements": [
                {"name": "input_tokens", "unit": "token", "classification": "measured", "value": 1800},
                {"name": "output_tokens", "unit": "token", "classification": "measured", "value": 600},
                {"name": "compression_rate_milli", "unit": "milliunit", "classification": "measured", "value": 333}
            ]
        }))
        .unwrap()
    };
    (invocation, observation)
}

fn key() -> ReceiptKey {
    ReceiptKey::from_bytes([7; 32])
}

#[test]
fn a_local_receipt_becomes_a_closed_document_without_refs_paths_or_digests() {
    for failed in [false, true] {
        let (invocation, observation) = local_receipt(failed);
        let (rcpt, document) =
            to_v2(&key(), "engine-receipt-local-1", &invocation, &observation).unwrap();
        document.validate().unwrap();
        let text = serde_json::to_string(&document).unwrap();
        for leak in [
            "Users",
            "ada",
            "payments",
            "source:",
            "recovery",
            "policy:",
            "output:",
            &"ab".repeat(32),
            &"cd".repeat(32),
            "engine-receipt-local-1",
        ] {
            assert!(!text.contains(leak), "{leak} leaked: {text}");
        }
        assert!(rcpt.as_str().starts_with("rcpt_"));
        assert_eq!(document.invocation.source_count, 2);
        assert_eq!(document.invocation.policy_admission.policy_digest, None);
        if failed {
            assert_eq!(document.observation.output_hmac, None);
            assert_eq!(
                serde_json::to_value(&document.observation.failure).unwrap(),
                json!({"code": "source_unavailable", "retryable_by_host": false})
            );
        } else {
            // `token` → `tokens`; a unit outside the contract is dropped.
            let names: Vec<&str> = document
                .observation
                .measurements
                .iter()
                .map(|m| m.name.as_str())
                .collect();
            assert_eq!(names, ["input_tokens", "output_tokens"]);
            assert_eq!(
                document.observation.measurements[0].unit,
                MeasurementUnit::Tokens
            );
        }
        // The engine's round trip: the closed type reads what it wrote.
        let back: EngineReceiptV2 = serde_json::from_str(&text).unwrap();
        assert_eq!(back, document);
    }
}

#[test]
fn input_and_output_are_keyed_so_hive_cannot_test_a_guess() {
    let (invocation, observation) = local_receipt(false);
    let (_, ours) = to_v2(&key(), "r", &invocation, &observation).unwrap();
    let (_, again) = to_v2(&key(), "r", &invocation, &observation).unwrap();
    let (_, other) = to_v2(
        &ReceiptKey::from_bytes([8; 32]),
        "r",
        &invocation,
        &observation,
    )
    .unwrap();
    assert_eq!(ours, again, "deterministic under one key (re-verifiable)");
    assert_ne!(ours.invocation.input_hmac, other.invocation.input_hmac);
    assert_ne!(ours.observation.output_hmac, other.observation.output_hmac);
    // Not an unsalted hash of the digest either.
    use sha2::Digest as _;
    let plain = hex::encode(Sha256::digest(invocation.input_digest.as_str().as_bytes()));
    assert!(!ours.invocation.input_hmac.as_str().contains(&plain));
}

#[test]
fn the_receipt_key_is_created_once_and_stays_local() {
    let dir = tempfile::tempdir().unwrap();
    let first = ReceiptKey::load_or_create(dir.path()).unwrap();
    let second = ReceiptKey::load_or_create(dir.path()).unwrap();
    assert_eq!(first.0, second.0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(dir.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key file is private");
    }
}

#[test]
fn engine_lease_organization_and_release_must_all_allow_receipts() {
    let on = HiveConfig::default();
    let allowed = lease(vec![HiveRight::ReceiptsUpload], None);
    assert_eq!(blocked_by(true, &on, &allowed, None), None);
    assert_eq!(blocked_by(false, &on, &allowed, None), Some(NOT_RELEASED));
    let off = HiveConfig {
        receipts_upload: false,
        ..HiveConfig::default()
    };
    assert!(blocked_by(true, &off, &allowed, None).is_some_and(|r| r.contains("this engine")));
    assert!(
        blocked_by(true, &on, &lease(vec![], None), None).is_some_and(|r| r.contains("Enterprise"))
    );
    assert!(
        blocked_by(
            true,
            &on,
            &lease(vec![HiveRight::ReceiptsUpload], Some(false)),
            None
        )
        .is_some_and(|r| r.contains("workspace"))
    );
    assert!(blocked_by(true, &on, &allowed, Some("lse_1")).is_some_and(|r| r.contains("refused")));
    assert_eq!(blocked_by(true, &on, &allowed, Some("lse_0")), None);
}

fn instance_record() -> InstanceRecord {
    InstanceRecord {
        instance_id: "inst_8e3c0d5f2b4e4f609b8c1d2e3f4a5b6c".into(),
        org_id: "org_acme".into(),
        org_name: "acme".into(),
        label: "laptop".into(),
        url: "https://hive.example".into(),
        org_salt_id: "s".into(),
        enrolled_at: "2026-10-25T10:00:00Z".into(),
        web_url: None,
        org_kind: None,
        company_name: None,
    }
}

/// N2 (owner): receipts fall under "Share engine metrics". With every
/// receipts switch on, no verified policy bundle and no confirmation, the
/// shared rule keeps them off.
#[test]
fn receipts_follow_the_share_engine_metrics_rule() {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let rights = vec![HiveRight::ReceiptsUpload, HiveRight::MetricsUpload];
    let reason = status(
        &store,
        &HiveConfig::default(),
        &instance_record(),
        &lease(rights, None),
    );
    assert_eq!(reason, Some(SHARING_OFF));
    // The receipts' own switches are named first.
    let reason = status(
        &store,
        &HiveConfig::default(),
        &instance_record(),
        &lease(vec![], None),
    );
    assert!(reason.is_some_and(|r| r.contains("Enterprise")));
}

/// N1: stopping drops the metrics switches and buffer, the receipts marker
/// and every queued receipt.
#[test]
fn stopping_local_recording_drops_switches_and_queues() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("d");
    crate::core::hive::metrics_v2::secrets::enable(&data).unwrap();
    write_private(&data, ENABLED_FILE, b"1").unwrap();
    let outbox = Outbox::at(&data);
    let now = Utc::now();
    outbox
        .enqueue(CHANNEL_RECEIPTS, json!({"receipt_id": "rcpt_1"}), now)
        .unwrap();
    assert_eq!(outbox.list(CHANNEL_RECEIPTS).len(), 1);
    stop_local_recording(&data);
    assert!(!crate::core::hive::metrics_v2::secrets::enabled(&data));
    assert!(!data.join(ENABLED_FILE).is_file());
    assert!(outbox.list(CHANNEL_RECEIPTS).is_empty());
}

fn queued(payload: Value) -> OutboxItem {
    OutboxItem {
        id: "1".into(),
        channel: CHANNEL_RECEIPTS.into(),
        created_at: "2026-10-21T10:00:00.000Z".into(),
        attempts: 0,
        next_attempt_at: None,
        last_error: None,
        policy_hold: false,
        payload,
    }
}

#[test]
fn a_queued_receipt_names_its_vault_and_never_its_path() {
    let mut map = VaultMap::default();
    map.vaults.insert(
        "stable".into(),
        VaultRecord {
            vault_id: "0123456789abcdef0123456789abcdef".into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: vec!["/work/payments".into()],
        },
    );
    let (invocation, observation) = local_receipt(false);
    let (rcpt, document) = to_v2(&key(), "r", &invocation, &observation).unwrap();
    let item = queued(json!({
        "receipt_id": rcpt.as_str(),
        "root": "/work/payments",
        "created_at": "2026-10-21T10:00:00Z",
        "document": document,
    }));
    let wire = wire_item(&map, &item).unwrap();
    assert_eq!(wire.vault_id.as_str(), "0123456789abcdef0123456789abcdef");
    wire.validate().unwrap();
    assert!(
        !serde_json::to_string(&wire)
            .unwrap()
            .contains("/work/payments")
    );
    let mut elsewhere = item.clone();
    elsewhere.payload["root"] = json!("/work/other");
    assert_eq!(wire_item(&map, &elsewhere), None);
    // A v1 item an older engine queued is never sent.
    let v1 = queued(json!({
        "receipt_id": "engine-receipt-0123", "kind": "engine", "root": "/work/payments",
        "created_at": "2026-10-21T10:00:00Z",
        "document": {"schema_version": 1, "invocation": {}, "observation": {}},
    }));
    assert_eq!(wire_item(&map, &v1), None);
}

// ── C0 2.5 receipts fixtures ──────────────────────────────────────────────────

fn instance_a() -> InstanceKeys {
    InstanceKeys::from_seeds([0x49; 32], [0x01; 32])
}

fn verifies(request: &ReceiptsRequestV2) -> bool {
    use ed25519_dalek::Verifier as _;
    let Ok(input) = request.body().signing_input(&request.key_id) else {
        return false;
    };
    let Ok(bytes) = hex::decode(request.signature.as_str()) else {
        return false;
    };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(&bytes) else {
        return false;
    };
    instance_a()
        .signing_key()
        .verifying_key()
        .verify(&input, &signature)
        .is_ok()
}

#[test]
fn the_request_fixture_is_signed_like_ours() {
    let raw = fixture_value("routes/receipts_v2.request.json");
    let request: ReceiptsRequestV2 = serde_json::from_value(raw.clone()).unwrap();
    request.validate().unwrap();
    assert_eq!(serde_json::to_value(&request).unwrap(), raw);
    assert!(verifies(&request), "the fixture signature verifies");
    let ours = sign(&instance_a(), request.body()).unwrap();
    assert_eq!(ours, request, "signed like the fixture");
}

/// RFC 6902 `replace`/`add`/`remove` on object members.
fn patch(mut doc: Value, ops: &[Value]) -> Value {
    for op in ops {
        let path = op["path"].as_str().unwrap();
        let (parent, last) = path.rsplit_once('/').unwrap();
        let map = doc
            .pointer_mut(parent)
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("{path}"));
        if op["op"] == "remove" {
            map.remove(last);
        } else {
            map.insert(last.to_string(), op["value"].clone());
        }
    }
    doc
}

#[test]
fn every_invalid_receipts_fixture_is_refused_at_its_layer() {
    let dir = format!("{FIXTURES}routes/invalid");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("receipts_v2_") {
            continue;
        }
        let case: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let doc = patch(
            fixture_value(case["base"].as_str().unwrap()),
            case["patch"].as_array().unwrap(),
        );
        let parsed = serde_json::from_value::<ReceiptsRequestV2>(doc);
        match case["expect"]["layer"].as_str().unwrap() {
            "signature" => {
                let request = parsed.unwrap_or_else(|e| panic!("{name}: {e}"));
                request.validate().unwrap();
                assert!(!verifies(&request), "{name}: the signature must not verify");
            }
            _ => assert!(
                parsed.as_ref().map_or(true, |r| r.validate().is_err()),
                "{name} must be refused: {}",
                case["why"]
            ),
        }
        seen += 1;
    }
    assert!(seen >= 8, "the invalid fixtures were not found ({seen})");
}

#[test]
fn batches_stay_within_the_contract_bounds() {
    let (invocation, observation) = local_receipt(false);
    let mut items = Vec::new();
    for i in 0..(MAX_RECEIPTS_V2 + 3) {
        let (rcpt, document) = to_v2(&key(), &format!("r{i}"), &invocation, &observation).unwrap();
        let wire = ReceiptItemV2::new(
            rcpt,
            VaultId::new("0123456789abcdef0123456789abcdef").unwrap(),
            TimeStamp::new("2026-10-21T10:00:00Z").unwrap(),
            document,
        )
        .unwrap();
        items.push((wire, queued(Value::Null)));
    }
    let out = batches(items);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].len(), MAX_RECEIPTS_V2);
    for batch in out {
        let body = ReceiptsBodyV2 {
            schema: ReceiptsSchema,
            receipts: batch.into_iter().map(|(w, _)| w).collect(),
        };
        body.validate().unwrap();
        let request = sign(&instance_a(), body).unwrap();
        assert!(serde_json::to_vec(&request).unwrap().len() <= MAX_RECEIPTS_BODY_BYTES);
    }
}

/// K2 review 3 (NEW-2): an [hive] off switch set by `config set` stops
/// recording at once.
#[test]
fn off_switches_stop_recording_at_once() {
    let setup = |data: &Path| {
        crate::core::hive::metrics_v2::secrets::enable(data).unwrap();
        write_private(data, ENABLED_FILE, b"1").unwrap();
    };
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("d");
    setup(&data);
    apply_off_switches_in(&data, &HiveConfig::default());
    assert!(crate::core::hive::metrics_v2::secrets::enabled(&data));
    assert!(data.join(ENABLED_FILE).is_file(), "all on: nothing stops");

    let receipts_off = HiveConfig {
        receipts_upload: false,
        ..HiveConfig::default()
    };
    apply_off_switches_in(&data, &receipts_off);
    assert!(!data.join(ENABLED_FILE).is_file());
    assert!(crate::core::hive::metrics_v2::secrets::enabled(&data));

    for off in [
        HiveConfig {
            metrics_upload: false,
            ..HiveConfig::default()
        },
        HiveConfig {
            enabled: Some(false),
            ..HiveConfig::default()
        },
    ] {
        setup(&data);
        apply_off_switches_in(&data, &off);
        assert!(!crate::core::hive::metrics_v2::secrets::enabled(&data));
        assert!(!data.join(ENABLED_FILE).is_file());
    }
}

/// K2 review 3 (NEW-3): the recording check also needs the [hive]
/// switches on and a lease that has not lapsed.
#[test]
fn recording_needs_the_switches_and_a_live_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    assert!(
        !store.recording_allowed(&HiveConfig::default()),
        "no stored lease: lapsed"
    );
    let off = HiveConfig {
        metrics_upload: false,
        ..HiveConfig::default()
    };
    assert!(!store.recording_allowed(&off));
}
