// SPDX-License-Identifier: Apache-2.0
use super::test_support::*;
use super::*;
use ed25519_dalek::{Signer as _, SigningKey};
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::lease::{HivePlan, LEASE_SCHEMA, PolicySignerKeyV1};
use lean_ctx_protocol::hive::{KEYSET_SIGNATURE_DOMAIN, signing_input_without_signature};

const SIGNER: [u8; 32] = [0x4f; 32];
const HIVE: [u8; 32] = [0x53; 32];
const ROOT: [u8; 32] = [0x52; 32];

fn lease(signers: &[[u8; 32]]) -> HiveLeaseV1 {
    HiveLeaseV1 {
        schema: LEASE_SCHEMA.into(),
        lease_id: "lse_1".into(),
        org_id: "org_acme".into(),
        instance_id: "inst_1".into(),
        plan: HivePlan::Team,
        seats_limit: 25,
        rights: vec![],
        policy_signer_keys: signers
            .iter()
            .map(|s| PolicySignerKeyV1 {
                org: "org_acme".into(),
                ed25519_pub: public_b64(*s),
            })
            .collect(),
        issued_at: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
        expires_at: UtcTimestamp::new("2026-10-21T12:00:00Z").unwrap(),
        grace_until: UtcTimestamp::new("2026-10-27T12:00:00Z").unwrap(),
        server_time: UtcTimestamp::new("2026-10-20T12:00:00Z").unwrap(),
        org_settings: None,
    }
}

fn keyset() -> HiveKeySetV1 {
    let mut raw = serde_json::json!({
        "schema": lean_ctx_protocol::hive::lease::KEYSET_SCHEMA,
        "keys": [{"kid": "k1", "ed25519_pub": public_b64(HIVE), "not_after": "2099-01-01T00:00:00Z"}],
    });
    let input = signing_input_without_signature(KEYSET_SIGNATURE_DOMAIN, &raw).unwrap();
    raw["signature"] =
        Value::from(STANDARD.encode(SigningKey::from_bytes(&ROOT).sign(&input).to_bytes()));
    verify_keyset(
        &raw,
        &SigningKey::from_bytes(&ROOT).verifying_key().to_bytes(),
    )
    .unwrap()
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-20T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn policy(version: u64) -> Value {
    artifact(
        SIGNER,
        "org_acme",
        version,
        "2026-10-20T11:00:00Z",
        "2026-11-20T12:00:00Z",
        PACK,
    )
}

#[allow(clippy::needless_pass_by_value)] // takes the bundle a builder just made
fn check(raw: Value, signers: &[[u8; 32]]) -> Result<(PolicyState, FilterSet), String> {
    verify_bundle(
        &raw,
        &lease(signers),
        &keyset(),
        &PolicyState::default(),
        now(),
    )
}

#[test]
fn a_policy_signed_by_the_lease_pinned_key_is_accepted_with_its_filter_rules() {
    let (state, filters) = check(
        bundle(
            1,
            Some(policy(1)),
            vec![filter_pack(HIVE, "k1", "ch-ids", 3)],
        ),
        &[SIGNER],
    )
    .unwrap();
    assert_eq!(state.policy_version, 1);
    assert_eq!(state.org_state.as_deref(), Some("required"));
    assert_eq!(state.filter_packs.get("ch-ids"), Some(&3));
    let rules = filter_rules(Some(&serde_json::to_string(&filters).unwrap()));
    assert_eq!(rules.redaction.len(), 1);
    assert_eq!(rules.redaction[0].0, "hive:ahv");
    assert!(rules.blocked.is_empty());
    assert_eq!(rules.dropped, 0);
    assert_eq!(filter_rules(Some("not json")).dropped, 1);
    assert_eq!(filter_rules(None), FilterRules::default());
}

/// HIVE-RT-D5: the org's signature names its packs; Hive cannot drop,
/// roll back or swap them, and a refusal keeps the last good bundle.
#[test]
fn packs_the_signed_policy_requires_must_all_be_there_as_signed() {
    let pack = filter_pack(HIVE, "k1", "ch-ids", 3);
    let required = |version: u64, sha256: &str| {
        artifact_requiring(
            SIGNER,
            "org_acme",
            1,
            "2026-10-20T11:00:00Z",
            "2026-11-20T12:00:00Z",
            PACK,
            vec![RequiredFilterPackV1 {
                pack_id: "ch-ids".into(),
                version,
                sha256: sha256.into(),
            }],
        )
    };
    let sha = pack["sha256"].as_str().unwrap().to_string();
    let (state, filters) = check(
        bundle(1, Some(required(3, &sha)), vec![pack.clone()]),
        &[SIGNER],
    )
    .unwrap();
    assert_eq!(state.filter_packs.get("ch-ids"), Some(&3));
    assert_eq!(filters.packs.len(), 1);

    let missing = check(bundle(1, Some(required(3, &sha)), vec![]), &[SIGNER]).unwrap_err();
    assert_eq!(missing, "required filter pack ch-ids is missing");
    // Another pack in its place, signed by Hive, is still not the one.
    let swapped = filter_pack_with(
        HIVE,
        "k1",
        "ch-ids",
        3,
        serde_json::json!([{"id": "none", "kind": "regex", "pattern": "x^", "action": "redact"}]),
    );
    let refused = check(bundle(1, Some(required(3, &sha)), vec![swapped]), &[SIGNER]);
    assert_eq!(
        refused.unwrap_err(),
        "required filter pack ch-ids version 3 is not the one the policy signed"
    );
    let newer = filter_pack(HIVE, "k1", "ch-ids", 4);
    assert!(check(bundle(1, Some(required(3, &sha)), vec![newer]), &[SIGNER]).is_err());
    // The list is part of the signature: editing it breaks the policy.
    let mut forged = required(3, &sha);
    forged["statement"]["required_filter_packs"] = serde_json::json!([]);
    assert!(check(bundle(1, Some(forged), vec![]), &[SIGNER]).is_err());
    // A malformed list is refused even when signed.
    assert!(
        check(
            bundle(1, Some(required(0, &sha)), vec![pack.clone()]),
            &[SIGNER]
        )
        .is_err()
    );
    // Packs the policy does not list still verify as before.
    let extra = filter_pack_with(
        HIVE,
        "k1",
        "extra",
        1,
        serde_json::json!([{"id": "falcon", "kind": "regex", "pattern": "falcon", "action": "block"}]),
    );
    let (state, _) = check(
        bundle(1, Some(required(3, &sha)), vec![pack, extra]),
        &[SIGNER],
    )
    .unwrap();
    assert_eq!(state.filter_packs.len(), 2);
}

/// The runtime check on what is installed: the verified set passes; a
/// set edited, emptied or removed by hand does not.
#[test]
fn the_installed_set_must_hold_the_required_packs_as_signed() {
    let pack = filter_pack(HIVE, "k1", "ch-ids", 3);
    let artifact = artifact_requiring(
        SIGNER,
        "org_acme",
        1,
        "2026-10-20T11:00:00Z",
        "2026-11-20T12:00:00Z",
        PACK,
        vec![RequiredFilterPackV1 {
            pack_id: "ch-ids".into(),
            version: 3,
            sha256: pack["sha256"].as_str().unwrap().into(),
        }],
    );
    let (state, set) = check(bundle(1, Some(artifact.clone()), vec![pack]), &[SIGNER]).unwrap();
    let text = artifact.to_string();
    let installed = serde_json::to_string(&set).unwrap();
    assert_eq!(required_packs_installed(&text, Some(&installed)), Ok(()));
    assert!(required_packs_installed(&text, None).is_err());
    // The accepted state keeps the list; status answers per pack by the same rule.
    assert_eq!(state.required_filter_packs.len(), 1);
    assert_eq!(
        required_packs_status(&state.required_filter_packs, Some(&installed)),
        [true]
    );
    assert_eq!(
        required_packs_status(&state.required_filter_packs, None),
        [false]
    );
    let mut emptied = set.clone();
    emptied.packs.clear();
    assert!(
        required_packs_installed(&text, Some(&serde_json::to_string(&emptied).unwrap())).is_err()
    );
    // The applied patterns edited by hand, the signed bytes left alone.
    let mut weakened = set.clone();
    weakened.packs[0].patterns[0].pattern = "never-matches^".into();
    assert!(
        required_packs_installed(&text, Some(&serde_json::to_string(&weakened).unwrap())).is_err()
    );
    // Both edited alike: the bytes no longer hash to the signed digest.
    let mut rewritten = weakened;
    let bytes = serde_json::to_vec(&rewritten.packs[0]).unwrap();
    rewritten
        .statements
        .insert("ch-ids".into(), STANDARD.encode(bytes));
    assert!(
        required_packs_installed(&text, Some(&serde_json::to_string(&rewritten).unwrap())).is_err()
    );
    // A shadow pack before the genuine one, reusing its rule labels with
    // patterns that never match: the genuine rules would be dropped.
    let mut shadowed = set.clone();
    let mut shadow = shadowed.packs[0].clone();
    shadow.pack_id = "aaa-shadow".into();
    for pattern in &mut shadow.patterns {
        pattern.pattern = "x^".into();
    }
    shadowed.packs.insert(0, shadow);
    assert!(
        required_packs_installed(&text, Some(&serde_json::to_string(&shadowed).unwrap())).is_err()
    );
    assert_eq!(
        required_packs_status(
            &state.required_filter_packs,
            Some(&serde_json::to_string(&shadowed).unwrap())
        ),
        [false],
        "status follows the runtime rule: a shadowed set installs nothing"
    );
    let mut unlisted = set;
    unlisted.statements.clear();
    assert!(
        required_packs_installed(&text, Some(&serde_json::to_string(&unlisted).unwrap())).is_err()
    );
    // A policy without required packs asks nothing of the set.
    assert_eq!(
        required_packs_installed(&policy(1).to_string(), None),
        Ok(())
    );
}

#[test]
fn unpinned_signers_tampered_packs_and_wrong_orgs_are_refused() {
    assert!(
        check(bundle(1, Some(policy(1)), vec![]), &[[0x11; 32]]).is_err(),
        "signer not in lease"
    );
    assert!(
        check(bundle(1, Some(policy(1)), vec![]), &[]).is_err(),
        "no trust on first use"
    );
    let mut tampered = policy(1);
    tampered["pack_toml"] = Value::from(format!("{PACK}\n# changed"));
    assert!(
        check(bundle(1, Some(tampered), vec![]), &[SIGNER]).is_err(),
        "pack swapped"
    );
    let mut forged = policy(1);
    forged["statement"]["grace_seconds"] = Value::from(999_999);
    assert!(
        check(bundle(1, Some(forged), vec![]), &[SIGNER]).is_err(),
        "statement edited"
    );
    let other_org = artifact(
        SIGNER,
        "org_other",
        1,
        "2026-10-20T11:00:00Z",
        "2026-11-20T12:00:00Z",
        PACK,
    );
    assert!(
        check(bundle(1, Some(other_org), vec![]), &[SIGNER]).is_err(),
        "other organization"
    );
    let expired = artifact(
        SIGNER,
        "org_acme",
        1,
        "2026-09-01T00:00:00Z",
        "2026-10-01T00:00:00Z",
        PACK,
    );
    assert!(
        check(bundle(1, Some(expired), vec![]), &[SIGNER]).is_err(),
        "expired"
    );
    let early = artifact(
        SIGNER,
        "org_acme",
        1,
        "2026-10-21T00:00:00Z",
        "2026-11-21T00:00:00Z",
        PACK,
    );
    assert!(
        check(bundle(1, Some(early), vec![]), &[SIGNER]).is_err(),
        "not yet valid"
    );
}

#[test]
fn filter_packs_must_be_signed_valid_and_within_the_budget() {
    let mut forged = filter_pack(HIVE, "k1", "ch-ids", 3);
    forged["envelope"]["signature"] =
        filter_pack([0x22; 32], "k1", "ch-ids", 3)["envelope"]["signature"].clone();
    assert!(
        check(bundle(0, None, vec![forged]), &[]).is_err(),
        "forged signature"
    );
    assert!(
        check(
            bundle(0, None, vec![filter_pack(HIVE, "k9", "ch-ids", 3)]),
            &[]
        )
        .is_err(),
        "unknown kid"
    );
    let broken = filter_pack_with(
        HIVE,
        "k1",
        "bad",
        1,
        serde_json::json!([{"id": "x", "kind": "regex", "pattern": "(", "action": "redact"}]),
    );
    assert!(
        check(bundle(0, None, vec![broken]), &[]).is_err(),
        "pattern does not compile"
    );
    let unknown = filter_pack_with(
        HIVE,
        "k1",
        "bad",
        1,
        serde_json::json!([{"id": "x", "kind": "regex", "pattern": "a", "action": "shout"}]),
    );
    assert!(
        check(bundle(0, None, vec![unknown]), &[]).is_err(),
        "unknown action"
    );
    let twice = vec![
        filter_pack(HIVE, "k1", "a-pack", 1),
        filter_pack(HIVE, "k1", "b-pack", 1),
    ];
    assert!(
        check(bundle(0, None, twice), &[]).is_err(),
        "rule label repeats across packs"
    );
    let many = |prefix: &str| {
        Value::Array(
                (0..64)
                    .map(|i| serde_json::json!({"id": format!("{prefix}{i}"), "kind": "regex", "pattern": format!("token{i}"), "action": "block"}))
                    .collect(),
            )
    };
    let full = vec![
        filter_pack_with(HIVE, "k1", "p1", 1, many("a")),
        filter_pack_with(HIVE, "k1", "p2", 1, many("b")),
    ];
    assert!(
        check(bundle(0, None, full.clone()), &[]).is_ok(),
        "exactly the budget"
    );
    let over = [full, vec![filter_pack_with(HIVE, "k1", "p3", 1, many("c"))]].concat();
    assert!(
        check(bundle(0, None, over), &[]).is_err(),
        "over the budget is refused as a whole"
    );
}

#[test]
fn versions_only_move_forward() {
    let lease = lease(&[SIGNER]);
    let (accepted, _) = verify_bundle(
        &bundle(2, Some(policy(2)), vec![]),
        &lease,
        &keyset(),
        &PolicyState::default(),
        now(),
    )
    .unwrap();
    assert!(
        verify_bundle(
            &bundle(1, Some(policy(1)), vec![]),
            &lease,
            &keyset(),
            &accepted,
            now()
        )
        .is_err(),
        "downgrade"
    );
    let other_pack = artifact(
        SIGNER,
        "org_acme",
        2,
        "2026-10-20T11:00:00Z",
        "2026-11-20T12:00:00Z",
        "name = \"x\"\n",
    );
    assert!(
        verify_bundle(
            &bundle(2, Some(other_pack), vec![]),
            &lease,
            &keyset(),
            &accepted,
            now()
        )
        .is_err(),
        "same version, other bytes"
    );
    assert!(
        verify_bundle(
            &bundle(2, Some(policy(2)), vec![]),
            &lease,
            &keyset(),
            &accepted,
            now()
        )
        .is_ok(),
        "the same statement again"
    );
    assert!(
        verify_bundle(
            &bundle(0, None, vec![]),
            &lease,
            &keyset(),
            &accepted,
            now()
        )
        .is_err(),
        "silently dropped policy"
    );
}

#[test]
fn enforcement_installs_pins_and_enrolls_only_with_the_verifier() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let raw = bundle(1, Some(policy(1)), vec![]);
    let (mut state, _) = check(raw.clone(), &[SIGNER]).unwrap();
    let installed = crate::core::policy::org::store::installed_path().unwrap();
    let marker = crate::core::policy::org::marker::marker_path_read_only().unwrap();

    enforce(&raw, &mut state, false).unwrap();
    assert!(!state.enforced, "no verifier, nothing enforced");
    assert!(
        !installed.exists() && !marker.exists(),
        "and nothing written"
    );

    enforce(&raw, &mut state, true).unwrap();
    assert!(state.enforced);
    let artifact: OrgPolicyArtifactV2 =
        serde_json::from_slice(&std::fs::read(&installed).unwrap()).unwrap();
    assert_eq!(artifact.statement.policy_version, 1);
    let recorded =
        crate::core::policy::org::marker::parse(&std::fs::read_to_string(&marker).unwrap())
            .unwrap();
    assert_eq!(recorded.organization_id.as_str(), "org_acme");
    assert_eq!(recorded.entries.len(), 1);
    assert_eq!(
        crate::core::policy::org::trust::pin_for(
            "org_acme",
            &hex::encode(SigningKey::from_bytes(&SIGNER).verifying_key().to_bytes())
        )
        .unwrap(),
        crate::core::policy::org::trust::PinOutcome::AlreadyPinned
    );

    // A second pass never re-enrolls: the stored marker stays as it was.
    let before = std::fs::read(&marker).unwrap();
    enforce(&raw, &mut state, true).unwrap();
    assert_eq!(std::fs::read(&marker).unwrap(), before);

    // A signer trusted for another organization is refused, not repinned.
    let other = artifact_for_org("org_other");
    let mut foreign = state.clone();
    foreign.org_state = Some("required".into());
    assert!(enforce(&bundle(1, Some(other), vec![]), &mut foreign, true).is_err());
}

fn artifact_for_org(org: &str) -> Value {
    artifact(
        SIGNER,
        org,
        1,
        "2026-10-20T11:00:00Z",
        "2026-11-20T12:00:00Z",
        PACK,
    )
}

/// The policy (valid until 2026-11-20 12:00) signed again with
/// `grace_seconds`.
fn with_grace(grace_seconds: u64) -> OrgPolicyArtifactV2 {
    let mut artifact: OrgPolicyArtifactV2 = serde_json::from_value(policy(1)).unwrap();
    artifact.statement.grace_seconds = grace_seconds;
    let mut signing = ORG_POLICY_SIGNATURE_DOMAIN.as_bytes().to_vec();
    signing.extend(serde_json::to_vec(&artifact.statement).unwrap());
    artifact.signature = Ed25519Signature::new(
        URL_SAFE_NO_PAD.encode(SigningKey::from_bytes(&SIGNER).sign(&signing).to_bytes()),
    )
    .unwrap();
    artifact
}

/// SEC-FIX follow-up: an org-signed `grace_seconds` beyond chrono's range
/// (between i64::MAX / 1000 and u64::MAX) panicked in `Duration::seconds`
/// or `until + grace`. Such a grace now never runs out, and a normal one
/// still ends the policy.
#[test]
fn a_grace_beyond_the_representable_time_never_panics() {
    let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
    let until = at("2026-11-20T12:00:00Z");
    assert_eq!(grace_end(until, 3600), Some(at("2026-11-20T13:00:00Z")));
    for huge in [
        u64::try_from(i64::MAX / 1000).unwrap() + 1,
        u64::try_from(i64::MAX).unwrap(),
        u64::try_from(i64::MAX).unwrap() + 1,
        u64::MAX,
    ] {
        assert_eq!(grace_end(until, huge), None, "{huge}");
    }
    // The largest grace chrono holds still ends past `until`, but beyond
    // the representable time: no end either.
    assert_eq!(
        grace_end(until, u64::try_from(i64::MAX / 1000).unwrap()),
        None
    );

    let later = at("2030-01-01T00:00:00Z");
    let lease = lease(&[SIGNER]);
    assert!(verify_org_policy(&with_grace(3600), &lease, now()).is_ok());
    assert_eq!(
        verify_org_policy(&with_grace(3600), &lease, later),
        Err("the policy has expired".into())
    );
    for huge in [u64::try_from(i64::MAX).unwrap(), u64::MAX] {
        assert!(
            verify_org_policy(&with_grace(huge), &lease, later).is_ok(),
            "{huge}: a grace that never runs out keeps the policy"
        );
    }
}
