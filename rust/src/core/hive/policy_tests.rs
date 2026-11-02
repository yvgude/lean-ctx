// SPDX-License-Identifier: Apache-2.0
use super::test_support::*;
use super::*;
use ed25519_dalek::{Signer as _, SigningKey};
use lean_ctx_protocol::hive::{KEYSET_SIGNATURE_DOMAIN, signing_input_without_signature};

const HIVE: [u8; 32] = [0x53; 32];
const ROOT: [u8; 32] = [0x52; 32];

fn keyset() -> HiveKeySetV1 {
    let public = SigningKey::from_bytes(&HIVE).verifying_key();
    let mut raw = serde_json::json!({
        "schema": lean_ctx_protocol::hive::lease::KEYSET_SCHEMA,
        "keys": [{"kid": "k1", "ed25519_pub": STANDARD.encode(public.as_bytes()), "not_after": "2099-01-01T00:00:00Z"}],
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

#[allow(clippy::needless_pass_by_value)] // takes the bundle a builder just made
fn check(raw: Value) -> Result<(PolicyState, FilterSet), String> {
    verify_bundle(&raw, &keyset(), &PolicyState::default(), now())
}

/// A TOML org policy in a bundle: `statement`, `public_key`, `signature`
/// and `pack_toml`, signed by a key no lease pins.
fn toml_org_policy() -> Value {
    serde_json::json!({
        "statement": {"schema": "leanctx.org-policy/v2", "organization_id": "org_acme", "policy_version": 3,
                      "required_filter_packs": [{"pack_id": "ch-ids", "version": 9, "sha256": "0".repeat(64)}]},
        "public_key": "not-a-key", "signature": "not-a-signature", "pack_toml": "name = \"floor\"\n",
    })
}

#[test]
fn filter_packs_signed_by_a_hive_key_are_accepted_with_their_rules() {
    let (state, filters) =
        check(bundle(0, None, vec![filter_pack(HIVE, "k1", "ch-ids", 3)])).unwrap();
    assert_eq!(state.filter_packs.get("ch-ids"), Some(&3));
    assert_eq!((state.policy_version, state.org_state), (0, None));
    let rules = filter_rules(Some(&serde_json::to_string(&filters).unwrap()));
    assert_eq!(rules.redaction.len(), 1);
    assert_eq!(rules.redaction[0].0, "hive:ahv");
    assert!(rules.blocked.is_empty());
    assert_eq!(rules.dropped, 0);
    assert_eq!(filter_rules(Some("not json")).dropped, 1);
    assert_eq!(filter_rules(None), FilterRules::default());
}

/// Whatever a bundle carries in `org_policy`, `unpin` or `policy_version`
/// is not read, so neither an unverifiable policy nor the packs it lists refuse the
/// bundle; its filter packs apply as always.
#[test]
fn a_toml_org_policy_in_the_bundle_is_ignored_and_its_filter_packs_apply() {
    let mut raw = bundle(
        3,
        Some(toml_org_policy()),
        vec![filter_pack(HIVE, "k1", "ch-ids", 3)],
    );
    raw["unpin"] = serde_json::json!({"statement": {"policy_version": 4}});
    let (state, filters) = check(raw).unwrap();
    assert_eq!(state.filter_packs.get("ch-ids"), Some(&3));
    assert_eq!(filters.packs.len(), 1);
    assert_eq!((state.policy_version, state.org_state), (0, None));
    // Not even its shape is checked.
    let mut odd = bundle(1, None, vec![]);
    odd["org_policy"] = Value::from("text");
    assert!(check(odd).is_ok());
}

/// Org-policy fields in the stored state (a policy in force, the packs it
/// required) are never loaded: a bundle without the policy is accepted,
/// and the next save drops those fields.
#[test]
fn org_policy_fields_in_the_stored_state_never_refuse_a_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let policy_dir = dir.path().join("policy");
    std::fs::create_dir_all(&policy_dir).unwrap();
    let old = serde_json::json!({
        "etag": "\"b7\"", "policy_version": 7, "statement_digest": format!("sha256:{}", "a".repeat(64)),
        "org_state": "required", "not_after": "2026-11-20T12:00:00Z", "enforced": true,
        "filter_packs": {"ch-ids": 3}, "verified_at": "2026-10-19T12:00:00Z", "last_refusal": null,
        "required_filter_packs": [{"pack_id": "ch-ids", "version": 3, "sha256": "b".repeat(64)}],
    });
    std::fs::write(policy_dir.join("state.json"), old.to_string()).unwrap();
    let loaded = load_state(dir.path());
    assert_eq!((loaded.policy_version, loaded.org_state.clone()), (0, None));
    assert_eq!(loaded.etag.as_deref(), Some("\"b7\""));
    assert_eq!(loaded.filter_packs.get("ch-ids"), Some(&3));
    let (state, _) = verify_bundle(
        &bundle(0, None, vec![filter_pack(HIVE, "k1", "ch-ids", 3)]),
        &keyset(),
        &loaded,
        now(),
    )
    .unwrap();
    save_state(dir.path(), &state).unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(policy_dir.join("state.json")).unwrap()).unwrap();
    for dropped in [
        "policy_version",
        "statement_digest",
        "org_state",
        "not_after",
        "enforced",
        "required_filter_packs",
    ] {
        assert!(saved.get(dropped).is_none(), "{dropped}: {saved}");
    }
    // Even the packs it required may go.
    assert!(verify_bundle(&bundle(0, None, vec![]), &keyset(), &loaded, now()).is_ok());
}

/// The policy runtime's check on what is installed, for an org policy it
/// holds: the verified set passes; a set edited,
/// emptied or removed by hand does not.
#[test]
fn the_installed_set_must_hold_the_required_packs_as_signed() {
    let pack = filter_pack(HIVE, "k1", "ch-ids", 3);
    let artifact = serde_json::json!({"statement": {"required_filter_packs": [
        {"pack_id": "ch-ids", "version": 3, "sha256": pack["sha256"]},
    ]}});
    let (_, set) = check(bundle(0, None, vec![pack])).unwrap();
    let text = artifact.to_string();
    let installed = serde_json::to_string(&set).unwrap();
    assert_eq!(required_packs_installed(&text, Some(&installed)), Ok(()));
    assert!(required_packs_installed(&text, None).is_err());
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
    let mut unlisted = set;
    unlisted.statements.clear();
    assert!(
        required_packs_installed(&text, Some(&serde_json::to_string(&unlisted).unwrap())).is_err()
    );
    // A policy without required packs asks nothing of the set; no
    // statement at all is not a policy.
    let none = serde_json::json!({"statement": {"policy_version": 1}}).to_string();
    assert_eq!(required_packs_installed(&none, None), Ok(()));
    assert!(required_packs_installed("{}", None).is_err());
}

#[test]
fn filter_packs_must_be_signed_valid_and_within_the_budget() {
    let mut forged = filter_pack(HIVE, "k1", "ch-ids", 3);
    forged["envelope"]["signature"] =
        filter_pack([0x22; 32], "k1", "ch-ids", 3)["envelope"]["signature"].clone();
    assert!(
        check(bundle(0, None, vec![forged])).is_err(),
        "forged signature"
    );
    assert!(
        check(bundle(0, None, vec![filter_pack(HIVE, "k9", "ch-ids", 3)])).is_err(),
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
        check(bundle(0, None, vec![broken])).is_err(),
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
        check(bundle(0, None, vec![unknown])).is_err(),
        "unknown action"
    );
    let twice = vec![
        filter_pack(HIVE, "k1", "a-pack", 1),
        filter_pack(HIVE, "k1", "b-pack", 1),
    ];
    assert!(
        check(bundle(0, None, twice)).is_err(),
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
        check(bundle(0, None, full.clone())).is_ok(),
        "exactly the budget"
    );
    let over = [full, vec![filter_pack_with(HIVE, "k1", "p3", 1, many("c"))]].concat();
    assert!(
        check(bundle(0, None, over)).is_err(),
        "over the budget is refused as a whole"
    );
}

#[test]
fn filter_pack_versions_only_move_forward() {
    let (accepted, _) = check(bundle(0, None, vec![filter_pack(HIVE, "k1", "ch-ids", 3)])).unwrap();
    let older = verify_bundle(
        &bundle(0, None, vec![filter_pack(HIVE, "k1", "ch-ids", 2)]),
        &keyset(),
        &accepted,
        now(),
    );
    assert_eq!(
        older.unwrap_err(),
        "filter pack ch-ids version 2 is older than the accepted one"
    );
    for version in [3, 4] {
        assert!(
            verify_bundle(
                &bundle(0, None, vec![filter_pack(HIVE, "k1", "ch-ids", version)]),
                &keyset(),
                &accepted,
                now()
            )
            .is_ok(),
            "version {version}"
        );
    }
}
