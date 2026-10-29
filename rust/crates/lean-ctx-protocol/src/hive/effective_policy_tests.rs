// SPDX-License-Identifier: Apache-2.0
//! The effective policy against the C0 2.x policy fixtures.

use super::*;
use serde_json::json;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path)
}

fn bundle_raw() -> Value {
    fixture("routes/policy_bundle.v2.response.json")["effective_policy"].clone()
}

fn bundle() -> EffectivePolicyBundleV1 {
    serde_json::from_value(bundle_raw()).unwrap()
}

#[test]
fn the_signed_bundle_validates_and_its_digest_is_the_policy_digest() {
    let raw = bundle_raw();
    let bundle = bundle();
    bundle.validate().unwrap();
    assert_eq!(policy_digest(&raw["policy"]).unwrap(), bundle.digest);
    assert_eq!(bundle.version, 3);
    assert_eq!(bundle.policy.vaults.len(), 1);
}

#[test]
fn the_signature_verifies_with_the_policy_key_over_the_raw_bundle() {
    use ed25519_dalek::{Signature, VerifyingKey};
    let keys = fixture("vectors/signing.vectors.json");
    let key = &keys["keys"]["hive_policy"];
    assert_eq!(key["key_id"], bundle_raw()["key_id"]);
    let mut public = [0u8; 32];
    for (i, byte) in public.iter_mut().enumerate() {
        let hex = &key["public_key_hex"].as_str().unwrap()[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(hex, 16).unwrap();
    }
    let key = VerifyingKey::from_bytes(&public).unwrap();
    let mut sig = [0u8; 64];
    let text = bundle_raw()["signature"].as_str().unwrap().to_string();
    for (i, byte) in sig.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap();
    }
    let signature = Signature::from_bytes(&sig);
    let input = signing_input_raw(&bundle_raw()).unwrap();
    assert!(input.starts_with(EFFECTIVE_POLICY_SIGNATURE_DOMAIN.as_bytes()));
    key.verify_strict(&input, &signature).unwrap();
    // Any changed member breaks it.
    let mut tampered = bundle_raw();
    tampered["policy"]["effective"]["context"]["max_context_tokens"] = json!(640_000);
    let input = signing_input_raw(&tampered).unwrap();
    assert!(key.verify_strict(&input, &signature).is_err());
}

#[test]
fn the_ack_reports_exactly_the_members_this_engine_does_not_enforce() {
    let request: PolicyAckRequestV1 =
        serde_json::from_value(fixture("routes/policy_ack.request.json")).unwrap();
    let ack = bundle().ack();
    assert_eq!(ack.version, request.version);
    assert_eq!(ack.digest, request.digest);
    // The C0 request is an engine's example ack: it names what that engine
    // cannot enforce (C0 c/follow-final: `context.max_context_tokens`).
    // This engine enforces that member, and Hive enforces the
    // `x-enforcement: server` ones (POLICY-B3, lead decision 26.10.), so it
    // reports only what is enforced nowhere.
    let mut want: Vec<String> = request
        .unenforced
        .iter()
        .filter(|m| !is_server_member(m) && !is_engine_member(m))
        .cloned()
        .collect();
    want.sort();
    assert_eq!(ack.unenforced, want, "same members, sorted bytewise");
    let mut with_advisory = bundle_raw();
    with_advisory["policy"]["effective"]["context"]["ranking_profile"] = json!("recent");
    with_advisory["policy"]["effective"]["security"]["quarantine_uploads"] = json!(true);
    let ack2 = serde_json::from_value::<EffectivePolicyBundleV1>(with_advisory)
        .unwrap()
        .ack();
    assert_eq!(
        ack2.unenforced,
        ["context.ranking_profile", "security.quarantine_uploads"],
        "advisory and unknown members, sorted bytewise"
    );
    // The engine sends it sorted, so its bytes are deterministic.
    assert_eq!(
        serde_json::to_value(&ack2).unwrap()["unenforced"],
        json!(ack2.unenforced)
    );
    let response: PolicyAckResponseV1 =
        serde_json::from_value(fixture("routes/policy_ack.response.json")).unwrap();
    response.validate_for(&ack).unwrap();
    assert_eq!(response.state, PolicyAckStateV1::Acked);
    let other = PolicyAckRequestV1 {
        version: 4,
        ..ack.clone()
    };
    assert!(response.validate_for(&other).is_err());
    // A mismatch is an error body the engine reads by its code.
    let error = fixture("routes/policy_ack.error.policy_mismatch.json");
    assert!(serde_json::from_value::<crate::hive::HiveErrorBody>(error.clone()).is_ok());
    assert_eq!(error["error"]["code"], POLICY_MISMATCH);
}

#[test]
fn every_member_of_the_schema_is_classified_once_as_its_x_enforcement_says() {
    let schema = fixture("schemas/policy.schema.json");
    let mut seen = BTreeSet::new();
    for section in ["security", "context", "sharing"] {
        for def in [section.to_string(), format!("{section}_effective")] {
            let props = schema["$defs"][&def]["properties"].as_object().unwrap();
            for (name, prop) in props {
                let member = format!("{section}.{name}");
                match prop["x-enforcement"].as_str().unwrap() {
                    "engine" => {
                        let mapped = ENGINE_MEMBERS.iter().find(|(m, _)| *m == member);
                        assert_eq!(
                            mapped.map(|(_, e)| *e),
                            prop["x-engine"].as_str(),
                            "{member}"
                        );
                        assert!(!ADVISORY_MEMBERS.contains(&member.as_str()), "{member}");
                        assert!(!is_server_member(&member), "{member}");
                    }
                    "server" => {
                        // Hive enforces it at its `x-server` point; this
                        // engine does not, so the ack reports it.
                        assert!(is_server_member(&member), "{member}");
                        assert!(!is_engine_member(&member), "{member}");
                        assert!(!ADVISORY_MEMBERS.contains(&member.as_str()), "{member}");
                        assert!(
                            prop["x-server"].as_str().is_some_and(|s| !s.is_empty()),
                            "{member} names its server point"
                        );
                        assert!(prop.get("x-engine").is_none(), "{member}");
                    }
                    "advisory" => {
                        assert!(ADVISORY_MEMBERS.contains(&member.as_str()), "{member}");
                        assert!(!is_engine_member(&member), "{member}");
                        assert!(!is_server_member(&member), "{member}");
                    }
                    other => panic!("{member}: {other}"),
                }
                seen.insert(member);
            }
        }
    }
    assert_eq!(
        seen.len(),
        ENGINE_MEMBERS.len() + SERVER_MEMBERS.len() + ADVISORY_MEMBERS.len()
    );
}

#[test]
fn the_ack_never_reports_server_members() {
    // Hive enforces a server member at its `x-server` point whatever the
    // engine does: it is not "enforced nowhere", so no ack lists it.
    let bundle = bundle();
    let server = server_members(&bundle.policy);
    assert!(!server.is_empty(), "the fixture bundle sets server members");
    assert!(server.iter().all(|m| is_server_member(m)));
    let ack = bundle.ack();
    assert!(ack.unenforced.iter().all(|m| !is_server_member(m)));
    assert!(ack.unenforced.iter().all(|m| !is_engine_member(m)));
    let mut sorted = SERVER_MEMBERS.to_vec();
    sorted.sort_unstable();
    assert_eq!(sorted, SERVER_MEMBERS, "kept sorted bytewise");
}

/// SERVER-B4: a refused bundle is reported with the one the engine keeps;
/// the C0 request and answer round-trip and pair up.
#[test]
fn a_refusal_report_matches_the_contract() {
    let raw = fixture("routes/policy_ack.refused.request.json");
    let request: PolicyAckRequestV1 = serde_json::from_value(raw.clone()).unwrap();
    let refused = request.refused.clone().unwrap();
    assert_eq!(refused.reason_code, PolicyRefusalReasonV1::KeyUnavailable);
    assert!(request.unenforced.is_empty());
    assert_eq!(serde_json::to_value(&request).unwrap(), raw, "round trip");
    let built = PolicyAckRequestV1::refusal(
        3,
        request.digest.clone(),
        PolicyAckRefusalV1 {
            reason_code: PolicyRefusalReasonV1::KeyUnavailable,
            detail: refusal_detail("effective policy: no valid policy key for this key_id"),
            applied_version: Some(2),
            applied_digest: Some(format!("sha256:{}", "2".repeat(64))),
        },
    );
    assert_eq!(built, request);
    let answer: PolicyAckRefusedResponseV1 =
        serde_json::from_value(fixture("routes/policy_ack.refused.response.json")).unwrap();
    answer.validate_for(&request).unwrap();
    let mut other = request.clone();
    other.refused.as_mut().unwrap().reason_code = PolicyRefusalReasonV1::Downgrade;
    assert!(answer.validate_for(&other).is_err());
    // No bundle kept: both applied members are null on the wire.
    let none = PolicyAckRefusalV1 {
        reason_code: PolicyRefusalReasonV1::InvalidBundle,
        detail: None,
        applied_version: None,
        applied_digest: None,
    };
    assert_eq!(
        serde_json::to_value(&none).unwrap(),
        json!({"reason_code": "invalid_bundle", "applied_version": null, "applied_digest": null})
    );
    // Every reason of the schema is known, and detail is bounded and clean.
    let schema = fixture("schemas/policy.schema.json");
    for code in schema["$defs"]["refusal_reason"]["enum"]
        .as_array()
        .unwrap()
    {
        assert!(
            serde_json::from_value::<PolicyRefusalReasonV1>(code.clone()).is_ok(),
            "{code}"
        );
    }
    assert_eq!(refusal_detail("a\u{1b}[31mb\n").as_deref(), Some("a [31mb"));
    assert_eq!(
        refusal_detail(&"x".repeat(400)).unwrap().len(),
        MAX_REFUSAL_DETAIL
    );
    assert_eq!(refusal_detail(" \n "), None);
}

#[test]
fn a_path_with_spaces_is_scrubbed_as_one_run() {
    assert_eq!(
        refusal_detail(
            "install the effective policy: write effective.json in /Users/John Smith/.local/share/lean-ctx: Permission denied"
        )
        .as_deref(),
        Some("install the effective policy: write effective.json in <path>: Permission denied")
    );
    assert_eq!(
        refusal_detail(r"open C:\Users\Jane Doe\hive failed").as_deref(),
        Some("open <path>")
    );
    assert_eq!(
        refusal_detail("no valid policy key for this key_id").as_deref(),
        Some("no valid policy key for this key_id")
    );
}

#[test]
fn every_merge_vector_result_is_a_valid_effective_policy_with_its_digest() {
    let vectors = fixture("vectors/policy_merge.vectors.json");
    let mut results = 0;
    for case in vectors["cases"].as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let Some(raw) = case.get("effective") else {
            assert!(case.get("error").is_some(), "{name}");
            continue;
        };
        let effective: EffectivePolicyV1 = serde_json::from_value(raw.clone()).expect(name);
        effective.validate().expect(name);
        assert_eq!(
            policy_digest(raw).unwrap(),
            case["effective_digest"].as_str().unwrap(),
            "{name}"
        );
        // The engine's typed view keeps every member: nothing is dropped
        // on the way to the floor.
        assert_eq!(&serde_json::to_value(&effective).unwrap(), raw, "{name}");
        results += 1;
    }
    assert!(results >= 10);
    // bundle_chain is the input of the signed bundle's vault entry.
    let chain = vectors["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["case"] == "bundle_chain")
        .unwrap();
    assert_eq!(
        chain["effective"],
        bundle_raw()["policy"]["vaults"][0]["effective"]
    );
}

/// A workspace linked to an organisation gets the organisation's own
/// org-level document as source `{level: org, scope: orgn_…}` (C0
/// `$defs/source`, web `policies-effective.json`): accepted at level `org`
/// only (it refused every such bundle: "a policy source is malformed").
#[test]
fn an_organisation_source_is_accepted_at_the_org_level_only() {
    let web = fixture("web/policies-effective.json");
    let sources = web["response"]["body"]["sources"].clone();
    assert!(
        sources
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["level"] == "org" && s["scope"].as_str().unwrap().starts_with("orgn_")),
        "the contract fixture carries an organisation source"
    );
    let mut raw = bundle_raw();
    raw["sources"] = sources;
    let bundle: EffectivePolicyBundleV1 = serde_json::from_value(raw.clone()).unwrap();
    bundle.validate().unwrap();
    // The same scope at another level is not the contract's.
    let mut other = raw;
    for source in other["sources"].as_array_mut().unwrap() {
        if source["scope"].as_str().unwrap().starts_with("orgn_") {
            source["level"] = json!("workspace");
        }
    }
    let bundle: EffectivePolicyBundleV1 = serde_json::from_value(other).unwrap();
    assert!(bundle.validate().is_err());
    for scope in [
        "orgn_9E8D7C6B5A4F30211F2E3D4C5B6A7980",
        "orgn_9e8d",
        "orgx_9e8d7c6b5a4f30211f2e3d4c5b6a7980",
    ] {
        let mut raw = bundle_raw();
        raw["sources"] = json!([{"level": "org", "scope": scope, "version": 1,
            "digest": "sha256:5555555555555555555555555555555555555555555555555555555555555555"}]);
        let bundle: EffectivePolicyBundleV1 = serde_json::from_value(raw).unwrap();
        assert!(bundle.validate().is_err(), "{scope}");
    }
}

#[test]
fn a_known_member_outside_the_contract_refuses_the_bundle() {
    let cases = [
        ("/policy/effective/context/max_context_tokens", json!(0)),
        ("/policy/effective/security/pii", json!("quarantine")),
        ("/policy/effective/context/denied_tools", json!(["b", "a"])),
        ("/policy/effective/context/denied_tools", json!(["a", "a"])),
        ("/policy/effective/context/read_mode", json!("everything")),
        ("/policy/effective/schema", json!("leanctx.hive.policy/v2")),
        ("/version", json!(0)),
        ("/org_id", json!("org_acme")),
        ("/signature", json!("AB")),
        ("/not_after", json!("2026-10-25T12:00:00Z")),
    ];
    for (path, value) in cases {
        let mut raw = bundle_raw();
        let (parent, key) = path.rsplit_once('/').unwrap();
        raw.pointer_mut(parent).unwrap()[key] = value;
        let refused = serde_json::from_value::<EffectivePolicyBundleV1>(raw)
            .map_err(|e| e.to_string())
            .and_then(|b| b.validate().map_err(|e| e.to_string()));
        assert!(refused.is_err(), "{path} must be refused");
    }
}

#[test]
fn an_unknown_member_is_kept_and_reported_as_unenforced() {
    let mut raw = bundle_raw();
    raw["policy"]["effective"]["security"]["quarantine_uploads"] = json!(true);
    let bundle: EffectivePolicyBundleV1 = serde_json::from_value(raw).unwrap();
    bundle.validate().unwrap();
    assert!(
        bundle
            .ack()
            .unenforced
            .contains(&"security.quarantine_uploads".to_string())
    );
    let vault = bundle
        .policy
        .for_vault(Some("3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42"));
    assert_eq!(vault.security.pii, Some(PolicyActionV1::Block));
    assert_eq!(
        bundle.policy.for_vault(None).security.pii,
        Some(PolicyActionV1::Redact)
    );
    assert_eq!(
        bundle
            .policy
            .for_vault(Some("00000000000000000000000000000000")),
        &bundle.policy.effective
    );
}
