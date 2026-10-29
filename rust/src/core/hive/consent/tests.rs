// SPDX-License-Identifier: Apache-2.0
//! Consent broker against the C0 2.0 consent fixtures and vectors
//! (T-CONSENT-4 record integrity, T-CONSENT-7 coverage, enum = fixture).

use super::broker::{Approval, BoundPresenceKey, Restart, verify_statement};
use super::channel::{is_agent_command, tty_preconditions};
use super::*;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};

const NOW: u64 = 1_792_800_000_000;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn seed(hex_seed: &str) -> SigningKey {
    let mut seed = [0u8; 32];
    hex::decode_to_slice(hex_seed, &mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

fn presence_a() -> (SigningKey, BoundPresenceKey) {
    let key = &fixture("vectors/signing.vectors.json")["keys"]["presence_a"];
    let signing = seed(key["seed_hex"].as_str().unwrap());
    let bound = BoundPresenceKey {
        alg: "ed25519".into(),
        key_id: fixture("vectors/consent_proof.vectors.json")["keys"]["presence_a"]["key_id"]
            .as_str()
            .unwrap()
            .into(),
        public_key: B64URL.encode(signing.verifying_key().to_bytes()),
    };
    (signing, bound)
}

fn consent_key() -> (String, VerifyingKey) {
    let key = &fixture("vectors/signing.vectors.json")["keys"]["hive_consent"];
    let mut public = [0u8; 32];
    hex::decode_to_slice(key["public_key_hex"].as_str().unwrap(), &mut public).unwrap();
    (
        key["key_id"].as_str().unwrap().into(),
        VerifyingKey::from_bytes(&public).unwrap(),
    )
}

fn web_trust() -> Trust {
    let (id, key) = consent_key();
    Trust {
        presence: None,
        consent_keys: BTreeMap::from([(id, key)]),
        owner_user_id: Some("2d4f6a8c-0e1b-4c3d-8e5f-7a9b1c3d5e7f".into()),
    }
}

fn broker(trust: Trust) -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    (dir, Broker::open(&store, trust))
}

fn case_subject(kind: Kind) -> Value {
    fixture("vectors/consent_subject.vectors.json")["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == kind.as_str())
        .unwrap()["subject"]
        .clone()
}

fn required_id(result: Result<Record, ConsentError>) -> String {
    match result {
        Err(ConsentError::Required { consent_id, .. }) => consent_id,
        other => panic!("expected consent_required, got {other:?}"),
    }
}

use std::collections::BTreeMap;

#[test]
fn the_embedded_kinds_equal_the_fixture_and_the_enum() {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/enums/consent-kinds.json",
        env!("CARGO_MANIFEST_DIR")
    );
    assert_eq!(KINDS_JSON, std::fs::read_to_string(path).unwrap());
    let listed: Vec<String> = fixture("enums/consent-kinds.json")["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["value"].as_str().unwrap().to_string())
        .collect();
    let ours: Vec<String> = Kind::ALL.iter().map(|k| k.as_str().to_string()).collect();
    assert_eq!(ours, listed);
    for kind in Kind::ALL {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        assert_eq!(Kind::parse(kind.as_str()), Some(kind));
    }
    let channels: Vec<Value> = fixture("enums/consent-channels.json")["values"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["value"].clone())
        .collect();
    let ours: Vec<Value> = [
        Channel::OsPresence,
        Channel::TtyOnly,
        Channel::Cockpit,
        Channel::Web,
    ]
    .iter()
    .map(|c| serde_json::to_value(c).unwrap())
    .collect();
    assert_eq!(ours, channels);
    assert!(!Kind::ApiKeySet.answerable_on("web") && Kind::ProcedureConfirm.answerable_on("web"));
    assert!(!Kind::ProcedureConfirm.answerable_on("cli"));
    assert!(Kind::SwarmJoin.server_gated() && !Kind::LinkVault.server_gated());
}

#[test]
fn subjects_digest_to_the_vectors() {
    for case in fixture("vectors/consent_subject.vectors.json")["cases"]
        .as_array()
        .unwrap()
    {
        let kind = Kind::parse(case["kind"].as_str().unwrap()).unwrap();
        check_subject(kind, &case["subject"]).unwrap_or_else(|e| panic!("{kind}: {e}"));
        let canonical = lean_ctx_protocol::hive::canonical_json(&case["subject"]).unwrap();
        assert_eq!(
            String::from_utf8(canonical).unwrap(),
            case["canonical"].as_str().unwrap()
        );
        assert_eq!(
            subject_digest(&case["subject"]).unwrap(),
            case["subject_digest"].as_str().unwrap()
        );
    }
}

#[test]
fn subjects_of_another_shape_are_refused() {
    let mut isolation = case_subject(Kind::SwarmJoin);
    isolation["isolation"] = "full".into();
    assert!(check_subject(Kind::SwarmJoin, &isolation).is_err(), "R107");
    assert!(check_subject(Kind::PledgeChange, &case_subject(Kind::SwarmJoin)).is_err());
    let mut bad = case_subject(Kind::ReviewerEnable);
    bad["reviewer_usd_cents_per_day"] = 99.into();
    assert!(check_subject(Kind::ReviewerEnable, &bad).is_err());
    let mut bad = case_subject(Kind::LinkVault);
    bad["target_vault"] = "acme/other".into();
    assert!(check_subject(Kind::LinkVault, &bad).is_err());
}

/// The macOS helper hashes subjects with its own BLAKE3 (Swift); its
/// `--selftest` prints this multi-chunk value, pinned here against the
/// reference crate.
#[test]
fn the_helper_blake3_multi_chunk_value_matches_the_reference() {
    assert_eq!(
        blake3::hash(&[b'a'; 5000]).to_hex().as_str(),
        "09d0d29a5f2dc69dff0809823ca867836c3a3cfb00e12df06d92e3f0f70629e9"
    );
}

#[test]
fn consent_ids_have_the_contract_shape() {
    let id = new_consent_id().unwrap();
    assert!(is_consent_id(&id), "{id}");
    assert!(is_consent_id("cns_cdefgh234567abcd"));
    assert!(!is_consent_id("cns_cdefgh234567abc1"));
    assert!(!is_consent_id("cns_../../etc/passwd"));
}

#[test]
fn a_presence_proof_verifies_against_the_vector() {
    let (_, bound) = presence_a();
    let vectors = fixture("vectors/consent_proof.vectors.json");
    let case = &vectors["cases"][0];
    let proof = fixture(case["fixture"].as_str().unwrap());
    assert_eq!(bound.key_id, proof["presence_key_id"]);
    let record = Record {
        consent_id: proof["consent_id"].as_str().unwrap().into(),
        kind: Kind::SwarmJoin,
        subject: proof["subject"].clone(),
        subject_digest: proof["subject_digest"].as_str().unwrap().into(),
        created_at: NOW,
        expires_at: NOW + REQUEST_TTL_MS,
        state: State::Approved,
        channel: Some(Channel::OsPresence),
        approved_at: proof["approved_at"].as_u64(),
        proof: Some(Proof::Presence(PresenceSignature {
            key_id: bound.key_id.clone(),
            alg: "ed25519".into(),
            signature: proof["signature"].as_str().unwrap().into(),
        })),
    };
    assert_eq!(
        String::from_utf8(record.proof_input(record.approved_at.unwrap()).unwrap()).unwrap(),
        case["signing_input"].as_str().unwrap()
    );
    let (_dir, with_key) = broker(Trust {
        presence: Some(bound.clone()),
        ..Trust::default()
    });
    assert!(with_key.verify(&record));
    // Another key, no bound key, or another approval time: refused.
    let mut other = bound;
    other.key_id = "f".repeat(16);
    let (_d2, wrong) = broker(Trust {
        presence: Some(other),
        ..Trust::default()
    });
    assert!(!wrong.verify(&record));
    let (_d3, unbound) = broker(Trust::default());
    assert!(!unbound.verify(&record));
    let mut later = record;
    later.approved_at = Some(later.approved_at.unwrap() + 1);
    assert!(!with_key.verify(&later));
}

#[test]
fn a_secure_enclave_p256_proof_verifies_against_the_vector() {
    let vectors = fixture("vectors/consent_proof.vectors.json");
    let key = &vectors["keys"]["presence_mac"];
    let bound = BoundPresenceKey {
        alg: "p256_ecdsa_sha256".into(),
        key_id: key["key_id"].as_str().unwrap().into(),
        public_key: key["public_key"].as_str().unwrap().into(),
    };
    let proof = fixture("consent/proof.p256_ecdsa_sha256.pledge_change.json");
    let record = Record {
        consent_id: proof["consent_id"].as_str().unwrap().into(),
        kind: Kind::PledgeChange,
        subject: proof["subject"].clone(),
        subject_digest: proof["subject_digest"].as_str().unwrap().into(),
        created_at: NOW,
        expires_at: NOW + REQUEST_TTL_MS,
        state: State::Approved,
        channel: Some(Channel::OsPresence),
        approved_at: proof["approved_at"].as_u64(),
        proof: Some(Proof::Presence(PresenceSignature {
            key_id: bound.key_id.clone(),
            alg: bound.alg.clone(),
            signature: proof["signature"].as_str().unwrap().into(),
        })),
    };
    let (_dir, broker) = broker(Trust {
        presence: Some(bound),
        ..Trust::default()
    });
    assert!(broker.verify(&record));
    let mut tampered = record;
    tampered.approved_at = Some(tampered.approved_at.unwrap() + 1);
    assert!(!broker.verify(&tampered));
}

#[test]
fn statements_verify_and_every_mismatch_is_refused() {
    let trust = web_trust();
    let statement = fixture("consent/statement.allow.json");
    let record = Record {
        consent_id: statement["consent_id"].as_str().unwrap().into(),
        kind: Kind::SwarmJoin,
        subject: json!({}),
        subject_digest: statement["subject_digest"].as_str().unwrap().into(),
        created_at: NOW,
        expires_at: statement["answered_at"].as_u64().unwrap() + 1,
        state: State::Pending,
        channel: None,
        approved_at: None,
        proof: None,
    };
    assert_eq!(
        verify_statement(&statement, &record, &trust),
        Ok(broker::Answer::Allow)
    );
    let refused = |doc: &Value, record: &Record, trust: &Trust| {
        verify_statement(doc, record, trust) == Err(ConsentError::RecordInvalid)
    };
    for case in ["statement_tampered", "statement_without_user"] {
        let invalid = fixture(&format!("consent/invalid/{case}.json"));
        let mut doc = statement.clone();
        for op in invalid["patch"].as_array().unwrap() {
            let field = op["path"].as_str().unwrap().trim_start_matches('/');
            match op["op"].as_str().unwrap() {
                "remove" => {
                    doc.as_object_mut().unwrap().remove(field);
                }
                _ => doc[field] = op["value"].clone(),
            }
        }
        assert!(refused(&doc, &record, &trust), "{case}");
    }
    let mut late = record.clone();
    late.expires_at = statement["answered_at"].as_u64().unwrap();
    assert!(refused(&statement, &late, &trust), "answered after expiry");
    let mut other = record.clone();
    other.consent_id = "cns_aaaaaaaaaaaaaaaa".into();
    assert!(refused(&statement, &other, &trust), "another request");
    let mut stranger = trust.clone();
    stranger.owner_user_id = Some("00000000-0000-4000-8000-000000000000".into());
    assert!(refused(&statement, &record, &stranger), "another account");
    assert!(
        refused(&statement, &record, &Trust::default()),
        "no consent key"
    );
    let deny = fixture("consent/statement.deny.json");
    let mut denied = record;
    denied.consent_id = deny["consent_id"].as_str().unwrap().into();
    denied.subject_digest = deny["subject_digest"].as_str().unwrap().into();
    denied.expires_at = u64::MAX;
    assert_eq!(
        verify_statement(&deny, &denied, &trust),
        Ok(broker::Answer::Deny)
    );
}

#[test]
fn a_tty_approval_on_a_tty_only_host_is_a_mac_record() {
    let (_dir, broker) = broker(Trust::default());
    let subject = case_subject(Kind::SealedGrant);
    let id = required_id(broker.require(Kind::SealedGrant, &subject, NOW));
    // The same subject reuses the pending request.
    assert_eq!(
        required_id(broker.require(Kind::SealedGrant, &subject, NOW + 1)),
        id
    );
    let approved = broker
        .approve(
            &id,
            Approval::TtyOnly {
                approved_at: NOW + 2,
            },
            NOW + 2,
        )
        .unwrap();
    assert!(matches!(approved.proof, Some(Proof::Mac(_))));
    assert_eq!(approved.channel, Some(Channel::TtyOnly));
    assert_eq!(
        broker
            .require(Kind::SealedGrant, &subject, NOW + 3)
            .unwrap(),
        approved
    );
    assert_eq!(
        broker.approve(&id, Approval::TtyOnly { approved_at: NOW }, NOW),
        Err(ConsentError::InvalidState)
    );
}

/// T-CONSENT-4: hand-written approvals are refused on every host.
#[test]
fn hand_written_approvals_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let tty = Broker::open(&store, Trust::default());
    let subject = case_subject(Kind::LinkVault);
    let id = required_id(tty.require(Kind::LinkVault, &subject, NOW));
    let path = dir.path().join("c/consent").join(format!("{id}.json"));
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["state"] = "approved".into();
    doc["channel"] = "tty_only".into();
    doc["approved_at"] = NOW.into();
    for proof in [
        Value::Null,
        json!({"mac": "00".repeat(32)}),
        json!({"presence": {"key_id": "ef1bd9c792ccc768", "alg": "ed25519", "signature": "AAAA"}}),
    ] {
        if proof.is_null() {
            doc.as_object_mut().unwrap().remove("proof");
        } else {
            doc["proof"] = proof;
        }
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(tty.get(&id), Err(ConsentError::RecordInvalid));
        assert_eq!(
            tty.require(Kind::LinkVault, &subject, NOW + 1),
            Err(ConsentError::RecordInvalid)
        );
    }
    // A genuine mac record stops counting once the host has a presence key.
    let fresh = tempfile::tempdir().unwrap();
    let store = crate::core::hive::HiveStore::at(fresh.path().join("c"), fresh.path().join("d"));
    let tty = Broker::open(&store, Trust::default());
    let id = required_id(tty.require(Kind::LinkVault, &subject, NOW));
    tty.approve(&id, Approval::TtyOnly { approved_at: NOW }, NOW)
        .unwrap();
    let (_, bound) = presence_a();
    let os = Broker::open(
        &store,
        Trust {
            presence: Some(bound),
            ..Trust::default()
        },
    );
    assert_eq!(os.get(&id), Err(ConsentError::RecordInvalid));
    // And a changed subject breaks the record too.
    let path = fresh.path().join("c/consent").join(format!("{id}.json"));
    let mut doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    doc["subject"]["vault"] = "acme/other".into();
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    assert_eq!(tty.get(&id), Err(ConsentError::RecordInvalid));
}

#[test]
fn a_presence_approval_is_checked_before_it_is_stored() {
    let (signing, bound) = presence_a();
    let (_dir, broker) = broker(Trust {
        presence: Some(bound.clone()),
        ..Trust::default()
    });
    let subject = case_subject(Kind::SwarmJoin);
    let id = required_id(broker.require(Kind::SwarmJoin, &subject, NOW));
    let record = broker.get(&id).unwrap();
    assert_eq!(
        broker.approve(&id, Approval::TtyOnly { approved_at: NOW }, NOW),
        Err(ConsentError::RecordInvalid),
        "no mac record on a host with a presence key"
    );
    let forged = Approval::Presence {
        signature: PresenceSignature {
            key_id: bound.key_id.clone(),
            alg: "ed25519".into(),
            signature: B64URL.encode(signing.sign(b"something else").to_bytes()),
        },
        approved_at: NOW,
        channel: Channel::OsPresence,
    };
    assert_eq!(
        broker.approve(&id, forged, NOW),
        Err(ConsentError::RecordInvalid)
    );
    assert_eq!(broker.get(&id).unwrap().state, State::Pending);
    let signed = Approval::Presence {
        signature: PresenceSignature {
            key_id: bound.key_id,
            alg: "ed25519".into(),
            signature: B64URL.encode(
                signing
                    .sign(&record.proof_input(NOW + 5).unwrap())
                    .to_bytes(),
            ),
        },
        approved_at: NOW + 5,
        channel: Channel::OsPresence,
    };
    broker.approve(&id, signed, NOW + 5).unwrap();
    assert_eq!(
        broker
            .require(Kind::SwarmJoin, &subject, NOW + 6)
            .unwrap()
            .state,
        State::Approved
    );
}

#[test]
fn kind_channel_and_expiry_rules() {
    let (_dir, broker) = broker(web_trust());
    let id = required_id(broker.require(
        Kind::ProcedureConfirm,
        &case_subject(Kind::ProcedureConfirm),
        NOW,
    ));
    assert_eq!(
        broker.approve(&id, Approval::TtyOnly { approved_at: NOW }, NOW),
        Err(ConsentError::WebOnly {
            consent_id: id.clone()
        })
    );
    let id = required_id(broker.require(Kind::ApiKeySet, &case_subject(Kind::ApiKeySet), NOW));
    assert_eq!(
        broker.approve(&id, Approval::Statement(json!({})), NOW),
        Err(ConsentError::TerminalOnly)
    );
    let subject = case_subject(Kind::PublicPresence);
    let id = required_id(broker.require(Kind::PublicPresence, &subject, NOW));
    let late = NOW + REQUEST_TTL_MS;
    assert_eq!(
        broker.approve(&id, Approval::TtyOnly { approved_at: late }, late),
        Err(ConsentError::Expired)
    );
    assert_eq!(broker.get(&id).unwrap().state, State::Expired);
    // An expired request is replaced by a new one.
    assert_ne!(
        required_id(broker.require(Kind::PublicPresence, &subject, late)),
        id
    );
}

#[test]
fn a_denial_blocks_until_the_request_would_have_expired() {
    let (_dir, broker) = broker(Trust::default());
    let subject = case_subject(Kind::ReviewerEnable);
    let id = required_id(broker.require(Kind::ReviewerEnable, &subject, NOW));
    broker.deny(&id, Channel::TtyOnly, NOW + 1).unwrap();
    assert_eq!(
        broker.require(Kind::ReviewerEnable, &subject, NOW + 2),
        Err(ConsentError::Denied)
    );
    assert_ne!(
        required_id(broker.require(Kind::ReviewerEnable, &subject, NOW + REQUEST_TTL_MS + 1)),
        id
    );
}

fn approve_now(broker: &Broker, kind: Kind, subject: &Value) -> Record {
    let id = required_id(broker.require(kind, subject, NOW));
    broker
        .approve(&id, Approval::TtyOnly { approved_at: NOW }, NOW)
        .unwrap()
}

/// T-CONSENT-7, restart half: the three restarts need no new request; a
/// CLI change on the review loop does.
#[test]
fn approved_records_cover_automatic_restarts() {
    let (_dir, broker) = broker(Trust::default());
    let review = case_subject(Kind::ReviewerEnable);
    approve_now(&broker, Kind::ReviewerEnable, &review);
    broker
        .cover(&Restart::ReviewLoop(review.clone()), NOW + 1)
        .unwrap();
    let mut other_cli = review.clone();
    other_cli["cli"] = "codex".into();
    required_id(broker.cover(&Restart::ReviewLoop(other_cli), NOW + 1));

    let unattended = case_subject(Kind::UnattendedStart);
    approve_now(&broker, Kind::UnattendedStart, &unattended);
    broker
        .cover(&Restart::Unattended(unattended.clone()), NOW + 1)
        .unwrap();
    let mut new_pledge = unattended;
    new_pledge["pledge_digest"] = "ab".repeat(32).into();
    required_id(broker.cover(&Restart::Unattended(new_pledge), NOW + 1));

    let join = case_subject(Kind::SwarmJoin);
    approve_now(&broker, Kind::SwarmJoin, &join);
    let participation = |pledge: &str| Restart::Participation {
        swarm: join["swarm"].as_str().unwrap().into(),
        cli: join["cli"].as_str().unwrap().into(),
        pledge_digest: pledge.into(),
    };
    broker
        .cover(
            &participation(join["pledge_digest"].as_str().unwrap()),
            NOW + 1,
        )
        .unwrap();
    let after = "9c".repeat(32);
    required_id(broker.cover(&participation(&after), NOW + 1));
    approve_now(
        &broker,
        Kind::PledgeChange,
        &json!({"swarm": join["swarm"], "before_digest": join["pledge_digest"], "after_digest": after}),
    );
    broker.cover(&participation(&after), NOW + 2).unwrap();

    // `swarm leave` ends the coverage.
    assert_eq!(
        broker
            .withdraw(
                &[Kind::SwarmJoin, Kind::PledgeChange, Kind::UnattendedStart],
                "swarm",
                join["swarm"].as_str().unwrap()
            )
            .unwrap(),
        3
    );
    required_id(broker.cover(&participation(&after), NOW + 3));
}

#[test]
fn the_tty_channel_refuses_pipes_agents_policy_and_presence_hosts() {
    let (_dir, broker) = broker(Trust::default());
    let id = required_id(broker.require(Kind::LinkVault, &case_subject(Kind::LinkVault), NOW));
    let record = broker.get(&id).unwrap();
    let tty = Trust::default();
    assert!(tty_preconditions(&record, &tty, false, true, None).is_ok());
    assert!(matches!(
        tty_preconditions(&record, &tty, false, false, None),
        Err(ConsentError::ChannelRefused(_))
    ));
    assert!(matches!(
        tty_preconditions(&record, &tty, false, true, Some("claude")),
        Err(ConsentError::ChannelRefused(_))
    ));
    assert_eq!(
        tty_preconditions(&record, &tty, true, true, None),
        Err(ConsentError::PresenceRequiredByPolicy {
            consent_id: id.clone()
        })
    );
    let (_, bound) = presence_a();
    let os = Trust {
        presence: Some(bound),
        ..Trust::default()
    };
    assert!(tty_preconditions(&record, &os, false, true, None).is_err());
    assert_eq!(record.typed_value(), "acme/payments");
    assert!(channel::prompt_text(&record).contains(record.fingerprint()));
}

#[test]
fn agent_commands_are_recognised() {
    for agent in [
        "claude",
        "/opt/homebrew/bin/codex exec",
        "cursor-agent",
        "gemini -p x",
        "copilot",
        "opencode",
        "pi",
        "node /usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        "node /home/u/.npm/bin/codex.js",
    ] {
        assert!(is_agent_command(agent), "{agent}");
    }
    for other in [
        "zsh",
        "-zsh",
        "/bin/bash",
        "node server.js",
        "tmux",
        "piper",
    ] {
        assert!(!is_agent_command(other), "{other}");
    }
}

/// No MCP action can approve: approval types are used only by the CLI
/// channel and this module (the test enumerates every source file).
#[test]
fn only_the_cli_and_the_broker_can_approve() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root.clone()];
    let mut offenders = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with("core/hive/consent/")
                || rel == "cli/hive_consent_cmd.rs"
                || rel.ends_with("/tests.rs")
                || rel.ends_with("_tests.rs")
            {
                // Test code approves fixtures; it is no MCP path.
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            if [
                "Approval::",
                "tty_approval",
                ".approve(&",
                "consent::broker::Approval",
            ]
            .iter()
            .any(|needle| text.contains(needle) && text.contains("consent"))
            {
                offenders.push(rel);
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "approval outside the CLI: {offenders:?}"
    );
}
