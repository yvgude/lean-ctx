// SPDX-License-Identifier: Apache-2.0

#[test]
fn canonical_encoding_and_digests_are_golden() {
    let checkpoint = fixture();
    let old_parent = format!("\"parent_checkpoint_id\":\"{PARENT_ID}\"");
    let new_parent = format!(
        "\"parent_branch_id\":\"main\",\"parent_checkpoint_id\":\"{PARENT_ID}\",\"parent_device_id\":\"device-a\",\"parent_device_sequence\":6"
    );
    let golden_base = GOLDEN_JSON.replace(&old_parent, &new_parent).replace(
        "\"task_id\":\"task-a\",\"workspace_id\":",
        "\"task_id\":\"task-a\",\"tenant_id\":\"tenant-a\",\"workspace_id\":",
    );
    let golden_json = format!(
        "{},\"updated_at\":\"2026-09-06T12:00:00Z\"}}",
        golden_base.strip_suffix('}').expect("golden object")
    );
    assert_eq!(checkpoint.canonical_json().expect("json"), golden_json);

    let inputs = checkpoint.digest_inputs().expect("digest inputs");
    assert_eq!(
        inputs.checkpoint_digest().as_str(),
        GOLDEN_CHECKPOINT_DIGEST
    );
    assert_eq!(inputs.identity_digest().as_str(), GOLDEN_IDENTITY_DIGEST);
    assert_eq!(inputs.lineage_digest().as_str(), GOLDEN_LINEAGE_DIGEST);
    assert_eq!(
        inputs.live_state_digest().as_str(),
        GOLDEN_LIVE_STATE_DIGEST
    );
    assert_eq!(
        checkpoint.digest().expect("digest").as_str(),
        GOLDEN_CHECKPOINT_DIGEST
    );

    let decoded = ContextCheckpointV1::from_canonical_bytes(golden_json.as_bytes())
        .expect("golden bytes decode");
    assert_eq!(decoded, checkpoint);
}

#[test]
fn every_digest_uses_a_distinct_separated_domain() {
    let domains = [
        CONTEXT_CHECKPOINT_DIGEST_DOMAIN,
        CONTEXT_CHECKPOINT_IDENTITY_DIGEST_DOMAIN,
        CONTEXT_CHECKPOINT_LINEAGE_DIGEST_DOMAIN,
        CONTEXT_CHECKPOINT_LIVE_STATE_DIGEST_DOMAIN,
        CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN,
    ];
    let unique = domains.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), domains.len());
    assert!(domains.iter().all(|domain| domain.ends_with(b"\0")));

    // The same canonical bytes under two domains must not collide.
    let bytes = fixture().canonical_bytes().expect("canonical bytes");
    let checkpoint_digest =
        digest_with_domain(CONTEXT_CHECKPOINT_DIGEST_DOMAIN, &bytes).expect("digest");
    let signature_digest =
        digest_with_domain(CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN, &bytes).expect("digest");
    assert_ne!(checkpoint_digest, signature_digest);
}

#[test]
fn signing_payload_is_sealed_and_domain_separated() {
    let checkpoint = fixture();
    let payload = checkpoint.signing_payload().expect("payload");
    assert!(
        payload
            .bytes()
            .starts_with(CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN)
    );
    assert_eq!(
        &payload.bytes()[CONTEXT_CHECKPOINT_SIGNATURE_DOMAIN.len()..],
        checkpoint.canonical_bytes().expect("bytes").as_slice()
    );
    assert_eq!(payload.digest().as_str(), GOLDEN_SIGNING_DIGEST);
}

#[test]
fn optional_session_learning_and_encryption_state_is_typed_and_round_trips() {
    let checkpoint = rich_fixture();
    let value = canonical_value(&checkpoint);
    assert!(value.pointer("/live_state/session_state").is_some());
    assert!(value.pointer("/live_state/learning_state").is_some());
    assert!(value.pointer("/encryption_metadata").is_some());
    assert_eq!(
        value
            .pointer("/live_state/session_state/identity/session_id")
            .and_then(Value::as_str),
        Some("session-a")
    );
    assert_eq!(
        value
            .pointer("/live_state/learning_state/state_ref")
            .and_then(Value::as_str),
        Some("learning:alpha")
    );
    assert_eq!(
        value
            .pointer("/encryption_metadata/algorithm")
            .and_then(Value::as_str),
        Some("aes256_gcm")
    );
    let decoded = decode(&value).expect("rich fixture decode");
    assert_eq!(decoded, checkpoint);
    assert_eq!(
        decoded.canonical_bytes().expect("rich bytes"),
        checkpoint.canonical_bytes().expect("rich bytes")
    );
}

#[test]
fn optional_state_rejects_unknown_fields_and_nonportable_references() {
    for pointer in [
        "/live_state/session_state",
        "/live_state/learning_state",
        "/encryption_metadata",
    ] {
        let mut value = canonical_value(&rich_fixture());
        value
            .pointer_mut(pointer)
            .expect("optional pointer")
            .as_object_mut()
            .expect("optional object")
            .insert("smuggled".to_owned(), json!(1));
        assert!(decode(&value).is_err(), "unknown field at {pointer}");
    }

    assert!(
        ContextCheckpointSessionStateV1::try_new(
            SessionIdentityV1 {
                session_id: SessionId::new("/tmp/session").expect("bounded session id"),
                ..session_state().identity
            },
            session_state().state,
        )
        .is_err()
    );
    assert!(
        ContextCheckpointLearningStateV1::try_new(
            reference("file:src/main.rs"),
            digest_of("9"),
            1,
        )
        .is_err()
    );
    assert!(
        ContextCheckpointEncryptionMetadataV1::try_new(
            ContextCheckpointEncryptionAlgorithmV1::Aes256Gcm,
            reference("secret:key"),
            digest_of("0"),
        )
        .is_err()
    );

    let mut aborted = session_state();
    aborted.state.phase = ContextSessionPhaseV1::Aborted;
    aborted.state.active_plan_id = None;
    aborted.state.abort_reason = Some(reference("http:169.254.169.254"));
    assert!(aborted.validate().is_err());
}

#[test]
fn strict_decoding_rejects_unknown_fields_everywhere() {
    for pointer in [
        "",
        "/identity",
        "/lineage",
        "/live_state",
        "/live_state/task",
        "/live_state/progress",
        "/live_state/decisions/0",
        "/live_state/files/0",
        "/live_state/policy_pins/0",
        "/live_state/package_pins/0",
        "/carrier",
    ] {
        let mut value = canonical_value(&fixture());
        let target = if pointer.is_empty() {
            &mut value
        } else {
            value.pointer_mut(pointer).expect("pointer resolves")
        };
        target
            .as_object_mut()
            .expect("object")
            .insert("smuggled".to_owned(), json!(1));
        assert!(
            decode(&value).is_err(),
            "unknown field at {pointer:?} must be rejected"
        );
    }
}

#[test]
fn strict_decoding_rejects_unknown_enum_variants_and_versions() {
    for (pointer, replacement) in [
        ("/live_state/task/status", json!("bogus")),
        ("/live_state/decisions/0/status", json!("bogus")),
        ("/live_state/files/0/role", json!("bogus")),
        ("/schema_version", json!(2)),
        ("/live_state/schema_version", json!(2)),
    ] {
        let mut value = canonical_value(&fixture());
        *value.pointer_mut(pointer).expect("pointer resolves") = replacement;
        assert!(
            decode(&value).is_err(),
            "{pointer} must reject an unsupported value"
        );
    }
}

#[test]
fn decoding_rejects_non_canonical_json() {
    let checkpoint = fixture();
    let canonical = checkpoint.canonical_json().expect("json");
    assert!(ContextCheckpointV1::from_canonical_bytes(canonical.as_bytes()).is_ok());

    let value = canonical_value(&checkpoint);
    let pretty = serde_json::to_vec_pretty(&value).expect("pretty");
    assert!(ContextCheckpointV1::from_canonical_bytes(&pretty).is_err());

    let unsorted = format!(
        "{{\"created_at\":\"2026-09-06T12:00:00Z\",{}",
        &canonical[canonical.find("\"carrier\"").expect("carrier key")..]
    );
    assert!(ContextCheckpointV1::from_canonical_bytes(unsorted.as_bytes()).is_err());

    let duplicated = canonical.replacen(
        "{\"carrier\"",
        "{\"created_at\":\"2026-09-06T12:00:00Z\",\"carrier\"",
        1,
    );
    assert!(ContextCheckpointV1::from_canonical_bytes(duplicated.as_bytes()).is_err());

    let trailing = format!("{canonical} ");
    assert!(ContextCheckpointV1::from_canonical_bytes(trailing.as_bytes()).is_err());

    let mut explicit_null = canonical_value(&checkpoint);
    *explicit_null
        .pointer_mut("/updated_at")
        .expect("updated_at pointer") = Value::Null;
    assert!(serde_json::from_value::<ContextCheckpointV1>(explicit_null).is_err());

    let oversized = vec![b' '; MAX_CONTEXT_CHECKPOINT_ENCODED_BYTES + 1];
    assert!(ContextCheckpointV1::from_canonical_bytes(&oversized).is_err());
}

#[test]
fn direct_deserialization_validates_and_migrates_legacy_updated_at() {
    let mut legacy = canonical_value(&fixture());
    legacy
        .as_object_mut()
        .expect("checkpoint object")
        .remove("updated_at");
    let migrated: ContextCheckpointV1 =
        serde_json::from_value(legacy).expect("legacy v1 timestamp migrates");
    assert_eq!(migrated.updated_at, migrated.created_at);

    let mut invalid = canonical_value(&fixture());
    invalid["live_state"]["task"]["task_id"] = json!("task-other");
    assert!(serde_json::from_value::<ContextCheckpointV1>(invalid).is_err());

    let mut network_ref = canonical_value(&fixture());
    network_ref["lineage"]["evidence_refs"] = json!(["http:127.0.0.1"]);
    assert!(serde_json::from_value::<ContextCheckpointV1>(network_ref).is_err());

    let mut legacy_tenant = canonical_value(&fixture());
    legacy_tenant["lineage"]
        .as_object_mut()
        .expect("lineage object")
        .remove("tenant_id");
    assert!(
        serde_json::from_value::<ContextCheckpointV1>(legacy_tenant.clone()).is_err(),
        "legacy tenantless bytes require authenticated migration context"
    );
    let legacy_bytes = canonical_bytes_of(&legacy_tenant, "legacy fixture").expect("bytes");
    let migrated = ContextCheckpointV1::from_legacy_v1_canonical_bytes(
        &legacy_bytes,
        TenantId::new("tenant-a").expect("tenant"),
    )
    .expect("authenticated legacy migration");
    assert_eq!(migrated.lineage.tenant_id.as_str(), "tenant-a");
    let pretty_legacy = serde_json::to_vec_pretty(&legacy_tenant).expect("pretty legacy");
    assert!(
        ContextCheckpointV1::from_legacy_v1_canonical_bytes(
            &pretty_legacy,
            TenantId::new("tenant-a").expect("tenant"),
        )
        .is_err(),
        "legacy migration must require canonical input bytes"
    );
    assert!(
        ContextCheckpointV1::from_legacy_v1_canonical_bytes(
            &fixture().canonical_bytes().expect("current bytes"),
            TenantId::new("tenant-a").expect("tenant"),
        )
        .is_err(),
        "migration must not rewrite current tenant-bound checkpoints"
    );
}

#[test]
fn every_public_nested_dto_deserialization_runs_its_validator() {
    macro_rules! reject_mutation {
        ($type:ty, $value:expr, $pointer:literal, $replacement:expr) => {{
            let mut encoded = serde_json::to_value($value).expect("encode nested fixture");
            *encoded
                .pointer_mut($pointer)
                .expect("nested mutation pointer") = json!($replacement);
            assert!(
                serde_json::from_value::<$type>(encoded).is_err(),
                concat!(stringify!($type), " must validate direct deserialization")
            );
        }};
    }

    let checkpoint = rich_fixture();
    reject_mutation!(
        ContextCheckpointIdentityV1,
        checkpoint.identity.clone(),
        "/device_sequence",
        99
    );
    reject_mutation!(
        ContextCheckpointLineageV1,
        checkpoint.lineage.clone(),
        "/receipt_ids",
        ["receipt-a", "receipt-a"]
    );
    reject_mutation!(
        ContextCheckpointLiveStateV1,
        checkpoint.live_state.clone(),
        "/schema_version",
        2
    );
    reject_mutation!(
        ContextCheckpointSessionStateV1,
        checkpoint
            .live_state
            .session_state
            .clone()
            .expect("session fixture"),
        "/state/next_event_sequence",
        0
    );
    reject_mutation!(
        ContextCheckpointCarrierBindingV1,
        checkpoint.carrier.clone().expect("carrier fixture"),
        "/carrier_schema_id",
        "leanctx.context-checkpoint/v999"
    );
    reject_mutation!(
        ContextCheckpointProgressV1,
        checkpoint.live_state.progress.clone(),
        "/total_steps",
        0
    );
    reject_mutation!(
        ContextCheckpointLearningStateV1,
        checkpoint
            .live_state
            .learning_state
            .clone()
            .expect("learning fixture"),
        "/state_ref",
        "https://example.invalid/state"
    );
    reject_mutation!(
        ContextCheckpointEncryptionMetadataV1,
        checkpoint
            .encryption_metadata
            .clone()
            .expect("encryption fixture"),
        "/key_ref",
        "ssh:secret"
    );
}

#[test]
fn canonical_identifiers_are_validated() {
    assert!(ContextCheckpointIdV1::new(CHECKPOINT_ID).is_ok());
    for invalid in [
        "11111111-2222-4333-8444-55555555555",
        "11111111222243338444555555555555",
        "11111111-2222-4333-8444-5555555555GG",
        "11111111-2222-4333-8444-5555555555FF",
        "00000000-0000-0000-0000-000000000000",
        "",
    ] {
        assert!(
            ContextCheckpointIdV1::new(invalid).is_err(),
            "{invalid:?} must be rejected"
        );
    }

    assert!(ContextCheckpointBranchIdV1::new("release-4.0").is_ok());
    assert!(ContextCheckpointDeviceIdV1::new("device-a").is_ok());
    assert!(validate_checkpoint_identifier("project-a", "identity").is_ok());
    for invalid in ["Project-a", "project/child", "project..child", "project-ß"] {
        assert!(
            validate_checkpoint_identifier(invalid, "identity").is_err(),
            "{invalid:?} must be rejected as a noncanonical identity"
        );
    }
    for invalid in [
        "",
        "-leading",
        "Upper",
        "has space",
        "has/slash",
        "base64:aaaa",
        "hex:dead",
        &"a".repeat(MAX_CONTEXT_CHECKPOINT_SLUG_BYTES + 1),
    ] {
        assert!(
            ContextCheckpointBranchIdV1::new(invalid).is_err(),
            "{invalid:?} must be rejected as a branch"
        );
        assert!(
            ContextCheckpointDeviceIdV1::new(invalid).is_err(),
            "{invalid:?} must be rejected as a device"
        );
    }

    for invalid in [
        "no-scheme",
        ":empty-scheme",
        "scheme:",
        "UPPER:value",
        "scheme:with space",
        "scheme:..\\parent",
        "scheme:../parent",
        "scheme:src/file",
        "scheme:Upper",
        "scheme:café",
        "scheme:%2fsecret",
        "provider:opaque-handle",
        "cache:opaque-handle",
        "evidence://users/example/private/notes.md",
        "snapshot://home/example/.ssh/id_rsa",
        "gotcha://var/folders/example/tmp/item",
        "kms://home/example/.config/keys/master.key",
        "https:///home/example/.ssh/id_rsa",
        "https://c:/users/example/.aws/credentials",
        "https://localhost/tmp/process-state",
        "https://127.0.0.1:8080/cache",
        "ssh:host",
        "data:text",
        "javascript:alert",
        &format!(
            "scheme:{}",
            "a".repeat(MAX_CONTEXT_CHECKPOINT_REFERENCE_BYTES)
        ),
    ] {
        assert!(
            validate_checkpoint_reference(invalid, "reference").is_err(),
            "{invalid:?} must be rejected as a reference"
        );
    }
    assert!(validate_checkpoint_reference("receipt:sha256:abc", "reference").is_ok());
    assert!(validate_checkpoint_reference("artifact://engine/receipt", "reference").is_ok());
}

#[test]
fn secret_and_path_shaped_values_are_rejected() {
    for invalid in [
        "/Users/someone/project",
        "~/workspace",
        "./relative",
        "../parent",
        "C:/Windows/system32",
        "windows\\path",
        "/tmp/scratch",
        "/var/folders/xy",
        "path=/tmp/scratch",
        "see ~/workspace",
        "file:///Users/someone/private",
        "path=C:/Windows/system32",
        "resume project://x path=/users/alice/.ssh/id_rsa",
        "ref knowledge://a cwd=/home/bob/work",
        "see receipt://r tmp=c:/users/bob",
        "note :// path=/tmp/x",
        "resume note path:/users/alice/.ssh/id_rsa",
        "cwd:/home/bob/work",
        "resume at dir:/etc/passwd",
        "home:~/work is pinned",
        "ref knowledge://a cwd:/home/bob/work",
        "see receipt://r tmp:c:/users/bob",
        "note :// path:/tmp/x",
        "path=\"/tmp/x\"",
        "path='/home/bob/x'",
        "cwd=</home/bob/x>",
        "resume project:///home/bob/work",
        "ref project:///users/alice/.ssh/id_rsa",
        "see knowledge://a/home/bob/work",
        "ref project:///home/bob/work=1",
        "see project:///users/alice/.ssh/id_rsa=x",
        "resume path=/tmp/x://y",
        "state ~/work://y",
        "note path=/users/alice/.ssh/id_rsa://y",
        "note cwd:/home/bob/work://y",
        "note knowledge://a?/home/bob/work",
        "note project://a|/home/bob/work",
        "cafe\u{301}",
        "line\u{2028}separator",
        "line\u{2029}separator",
        "api_key=abcdef",
        "access_key=abcdef",
        "AWS_SECRET=abc",
        "password: hunter2",
        "Bearer abcdef",
        "-----BEGIN PRIVATE KEY-----",
        "session token=abc",
        "pid=4321",
        "provider_handle=opaque",
        "flush_timestamp=2026-09-06T12:00:00Z",
        "ghp_aaaaaaaaaaaaaaaaaaaa",
        "sk-aaaaaaaaaaaaaaaaaaaa",
        "xoxb-1-2-3",
        "base64:AAAA",
        "built from /opt/ci/workspace/artifacts",
        "see /usr/local/share/example",
        "plan A\u{202e}kcatta",
        "zero\u{200b}width",
        "see /opt",
        "see /usr",
        "key=abc",
        "bearer=abc",
        "bearer: abc",
        "key: abc",
        "x-api-key: abc",
    ] {
        assert!(
            ContextCheckpointTextV1::new(invalid).is_err(),
            "{invalid:?} must be rejected as checkpoint text"
        );
        assert!(
            reject_machine_local(invalid, "field").is_err(),
            "{invalid:?} must be rejected as machine-local"
        );
    }
    let jwt = ["eyJhbGci", "eyJzdWIi", "YWJjc2ln"].join(".");
    let credential_url = ["postgres://alice:", "example", "@db.invalid/app"].concat();
    let aws_access_key = ["AKIA", "1234567890ABCDEF"].concat();
    let aws_identity_key = ["AIDA", "1234567890ABCDEF"].concat();
    let punctuated_jwt = format!("{jwt}.");
    for invalid in [
        &jwt,
        &credential_url,
        &aws_access_key,
        &aws_identity_key,
        &punctuated_jwt,
    ] {
        assert!(
            ContextCheckpointTextV1::new(invalid).is_err(),
            "credential-shaped input must be rejected"
        );
    }
    for portable in [
        "risk-based ordering retained",
        "document secret-redaction policy",
        "update Cargo.lock deterministically",
        "explain why temporary directories are not durable",
        "provider cache remains disabled",
        "pid 4321 was observed but is not retained as state",
        "https://example.invalid is an evidence URL",
        "https://example.com/docs/page",
        "https://example.com/a?b=c&d=e",
        "https://example.com/x=1,y=2",
        "project://x is a stable identity",
        "knowledge://a and receipt://r are semantic references",
        "artifact://engine/receipt is portable",
        "ratio 3:8 with plan:alpha and task:beta",
        "and/or 3/8 src/lib.rs A/B token bucket key rotation",
    ] {
        assert!(
            ContextCheckpointTextV1::new(portable).is_ok(),
            "{portable:?} is semantic prose, not embedded local state"
        );
    }
}

#[test]
fn machine_local_material_is_rejected_inside_every_identity_slot() {
    let mut checkpoint = fixture();
    checkpoint.lineage.project_id = ProjectId::new("/Users/someone").expect("bounded id");
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.files[0].source_id = SourceId::new("/tmp/scratch").expect("bounded id");
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.profile_id = Some(ProfileId::new("token=abc").expect("bounded id"));
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.lineage.evidence_refs = vec![reference("file:///Users/someone")];
    assert!(checkpoint.validate().is_err());
}

#[test]
fn portable_state_rejects_network_endpoints_and_embedded_credentials() {
    for invalid in [
        "http://169.254.169.254/latest/meta-data",
        "https://example.invalid/evidence",
        "http://10.0.0.1/internal",
        "http:127.0.0.1",
        "http:169.254.169.254",
        "https:[::1]",
        "http:0x7f000001",
    ] {
        assert!(
            validate_checkpoint_reference(invalid, "reference").is_err(),
            "{invalid:?} must not become a portable network capability"
        );
    }
    assert!(validate_checkpoint_reference("artifact://engine/receipt", "reference").is_ok());

    let jwt = ["eyJhbGci", "eyJzdWIi", "YWJjc2ln"].join(".");
    for invalid in [
        "url=sk-examplecredential".to_owned(),
        "ref:ghp_examplecredential".to_owned(),
        format!("jwt={jwt}"),
        "token = abc".to_owned(),
        "password : abc".to_owned(),
        "api_key : abc".to_owned(),
        "ref:gho_examplecredential".to_owned(),
        "https:alice:secret@example".to_owned(),
        "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
        "see /".to_owned(),
    ] {
        assert!(
            ContextCheckpointTextV1::new(&invalid).is_err(),
            "{invalid:?} embedded credential fragment must be rejected"
        );
    }
}

#[test]
fn every_bounded_string_and_collection_enforces_its_cap() {
    assert!(ContextCheckpointTextV1::new("a".repeat(MAX_CONTEXT_CHECKPOINT_TEXT_BYTES)).is_ok());
    assert!(
        ContextCheckpointTextV1::new("a".repeat(MAX_CONTEXT_CHECKPOINT_TEXT_BYTES + 1)).is_err()
    );
    assert!(ContextCheckpointTextV1::new("  ").is_err());
    assert!(ContextCheckpointTextV1::new(" padded ").is_err());
    assert!(ContextCheckpointTextV1::new("line\nbreak").is_err());
    assert!(ContextCheckpointTextV1::new("\u{feff}mark").is_err());

    let decision = |index: usize| ContextCheckpointDecisionV1 {
        decision_id: DecisionId::new(format!("decision-{index:04}")).expect("valid decision"),
        statement: text("statement"),
        rationale: text("rationale"),
        status: ContextCheckpointDecisionStatusV1::Accepted,
        evidence_refs: Vec::new(),
    };
    let mut checkpoint = fixture();
    checkpoint.live_state.decisions = (0..=MAX_CONTEXT_CHECKPOINT_DECISIONS)
        .map(decision)
        .collect();
    assert!(checkpoint.validate().is_err());
    checkpoint.live_state.decisions = (0..MAX_CONTEXT_CHECKPOINT_DECISIONS)
        .map(decision)
        .collect();
    assert!(checkpoint.validate().is_ok());

    let mut checkpoint = fixture();
    checkpoint.live_state.decisions[0].evidence_refs = (0..=MAX_CONTEXT_CHECKPOINT_DECISION_REFS)
        .map(|index| reference(&format!("evidence:{index:04}")))
        .collect();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.findings = (0..=MAX_CONTEXT_CHECKPOINT_FINDINGS)
        .map(|index| text(&format!("finding {index}")))
        .collect();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.next_steps = (0..=MAX_CONTEXT_CHECKPOINT_NEXT_STEPS)
        .map(|index| text(&format!("step {index}")))
        .collect();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.files = (0..=MAX_CONTEXT_CHECKPOINT_FILES)
        .map(|index| ContextCheckpointFileV1 {
            source_id: SourceId::new(format!("source-{index:04}")).expect("valid source"),
            role: ContextCheckpointFileRoleV1::Input,
            content_digest: digest_of("3"),
        })
        .collect();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.policy_pins = (0..=MAX_CONTEXT_CHECKPOINT_POLICY_PINS)
        .map(|index| ContextCheckpointPolicyPinV1 {
            policy_id: PolicyId::new(format!("policy-{index:04}")).expect("valid policy"),
            policy_digest: digest_of("5"),
        })
        .collect();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.package_pins = (0..=MAX_CONTEXT_CHECKPOINT_PACKAGE_PINS)
        .map(|index| ContextCheckpointPackagePinV1 {
            package_id: PackageId::new(format!("package-{index:04}")).expect("valid package"),
            version: SemanticVersion::new("1.2.3").expect("valid version"),
            package_digest: digest_of("6"),
        })
        .collect();
    assert!(checkpoint.validate().is_err());

    for field in [
        "evidence_refs",
        "knowledge_refs",
        "gotcha_refs",
        "snapshot_refs",
    ] {
        let mut checkpoint = fixture();
        let overflow = (0..=MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS)
            .map(|index| reference(&format!("evidence:{index:04}")))
            .collect::<Vec<_>>();
        match field {
            "evidence_refs" => checkpoint.lineage.evidence_refs = overflow,
            "knowledge_refs" => checkpoint.lineage.knowledge_refs = overflow,
            "gotcha_refs" => checkpoint.lineage.gotcha_refs = overflow,
            _ => checkpoint.lineage.snapshot_refs = overflow,
        }
        assert!(checkpoint.validate().is_err(), "{field} cap must hold");
    }

    let mut checkpoint = fixture();
    checkpoint.lineage.receipt_ids = (0..=MAX_CONTEXT_CHECKPOINT_LINEAGE_REFS)
        .map(|index| ReceiptId::new(format!("receipt-{index:04}")).expect("valid receipt"))
        .collect();
    assert!(checkpoint.validate().is_err());
}

#[test]
fn ordering_and_uniqueness_rules_are_enforced() {
    let mut checkpoint = fixture();
    checkpoint.live_state.decisions.reverse();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.files.reverse();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.lineage.receipt_ids.reverse();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.lineage.evidence_refs.reverse();
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.findings = vec![text("same"), text("same")];
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.next_steps = vec![text("same"), text("same")];
    assert!(checkpoint.validate().is_err());
}

#[test]
fn progress_counters_are_bounded_and_consistent() {
    let mut checkpoint = fixture();
    checkpoint.live_state.progress.total_steps = 0;
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.progress.total_steps = MAX_CONTEXT_CHECKPOINT_STEPS + 1;
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.progress.completed_steps = checkpoint.live_state.progress.total_steps + 1;
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.progress.confidence_milliunits = 1_001;
    assert!(checkpoint.validate().is_err());

    let mut value = canonical_value(&fixture());
    *value
        .pointer_mut("/live_state/progress/confidence_milliunits")
        .expect("pointer") = json!(1_001);
    assert!(decode(&value).is_err());
}

#[test]
fn identity_sequence_and_parent_rules_are_enforced() {
    let checkpoint_id = ContextCheckpointIdV1::new(CHECKPOINT_ID).expect("valid id");
    let branch = ContextCheckpointBranchIdV1::new("main").expect("valid branch");
    let device = ContextCheckpointDeviceIdV1::new("device-a").expect("valid device");

    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            None,
            None,
            None,
            None,
            branch.clone(),
            device.clone(),
            1,
        )
        .is_ok()
    );
    // A root checkpoint may not claim a continued sequence.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            None,
            None,
            None,
            None,
            branch.clone(),
            device.clone(),
            2,
        )
        .is_err()
    );
    // Parent metadata is atomic; an ID alone cannot establish lineage.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            Some(ContextCheckpointIdV1::new(PARENT_ID).expect("valid parent")),
            None,
            None,
            None,
            branch.clone(),
            device.clone(),
            2,
        )
        .is_err()
    );
    // A device must advance exactly one sequence from its own parent.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            Some(ContextCheckpointIdV1::new(PARENT_ID).expect("valid parent")),
            Some(device.clone()),
            Some(branch.clone()),
            Some(1),
            branch.clone(),
            device.clone(),
            1,
        )
        .is_err()
    );
    // A first checkpoint on another device may continue a remote parent.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            Some(ContextCheckpointIdV1::new(PARENT_ID).expect("valid parent")),
            Some(ContextCheckpointDeviceIdV1::new("device-b").expect("device")),
            Some(branch.clone()),
            Some(9),
            branch.clone(),
            device.clone(),
            1,
        )
        .is_ok()
    );
    // The sequence is one-based.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            Some(ContextCheckpointIdV1::new(PARENT_ID).expect("valid parent")),
            Some(device.clone()),
            Some(branch.clone()),
            Some(1),
            branch.clone(),
            device.clone(),
            0,
        )
        .is_err()
    );
    // Same-device continuation must reject arithmetic overflow.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            Some(ContextCheckpointIdV1::new(PARENT_ID).expect("valid parent")),
            Some(device.clone()),
            Some(branch.clone()),
            Some(u64::MAX),
            branch.clone(),
            device.clone(),
            u64::MAX,
        )
        .is_err()
    );
    // A checkpoint may not be its own parent.
    assert!(
        ContextCheckpointIdentityV1::try_new(
            checkpoint_id.clone(),
            Some(checkpoint_id),
            Some(device.clone()),
            Some(branch.clone()),
            Some(1),
            branch,
            device,
            2,
        )
        .is_err()
    );
}

#[test]
fn cross_field_bindings_are_enforced() {
    let mut checkpoint = fixture();
    checkpoint.live_state.task.task_id = TaskId::new("task-other").expect("valid task");
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    checkpoint.live_state.task.plan_id = None;
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = fixture();
    let mut carrier = carrier();
    carrier.workspace_id =
        WorkspaceId::new("bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee").expect("valid workspace");
    checkpoint.carrier = Some(carrier);
    assert!(checkpoint.validate().is_err());
}

#[test]
fn session_state_is_bound_to_tenant_project_workspace_task_and_plan() {
    let mut checkpoint = rich_fixture();
    checkpoint.lineage.tenant_id = TenantId::new("tenant-b").expect("tenant");
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = rich_fixture();
    checkpoint
        .live_state
        .session_state
        .as_mut()
        .expect("session")
        .identity
        .tenant_id = Some(TenantId::new("tenant-b").expect("tenant"));
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = rich_fixture();
    checkpoint.lineage.plan_id = None;
    checkpoint.live_state.task.plan_id = None;
    checkpoint
        .live_state
        .session_state
        .as_mut()
        .expect("session")
        .state
        .active_plan_id = None;
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = rich_fixture();
    checkpoint
        .live_state
        .session_state
        .as_mut()
        .expect("session")
        .identity
        .workspace_id =
        Some(WorkspaceId::new("bbbbbbbb-bbbb-4ccc-8ddd-eeeeeeeeeeee").expect("workspace"));
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = rich_fixture();
    checkpoint
        .live_state
        .session_state
        .as_mut()
        .expect("session")
        .identity
        .task_id = TaskId::new("task-b").expect("task");
    assert!(checkpoint.validate().is_err());

    let mut checkpoint = rich_fixture();
    checkpoint
        .live_state
        .session_state
        .as_mut()
        .expect("session")
        .state
        .active_plan_id = Some(PlanId::new("plan-b").expect("plan"));
    assert!(checkpoint.validate().is_err());
}

#[test]
fn carrier_binding_matches_the_p6_v2_contract() {
    assert_eq!(
        CONTEXT_CHECKPOINT_V2_SCHEMA_ID,
        "leanctx.context-checkpoint-live/v2"
    );
    assert_ne!(
        CONTEXT_CHECKPOINT_V2_SCHEMA_ID, CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID,
        "live checkpoint and .ctxpkg carrier are distinct wire contracts"
    );
    assert_eq!(
        CONTEXT_CHECKPOINT_CARRIER_SCHEMA_ID,
        "leanctx.context-checkpoint/v2"
    );
    assert_eq!(
        CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_SCHEMA_ID,
        "leanctx.workspace.state/v1"
    );
    assert_eq!(
        CONTEXT_CHECKPOINT_CARRIER_STATE_DIGEST_DOMAIN,
        "leanctx.workspace.state.v1"
    );
    assert_eq!(
        CONTEXT_CHECKPOINT_CARRIER_ENVELOPE_DIGEST_DOMAIN,
        "leanctx.checkpoint.envelope.v2"
    );
    let mut sorted = CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_KEYS;
    sorted.sort_unstable();
    assert_eq!(sorted, CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_KEYS);
    assert_eq!(
        CONTEXT_CHECKPOINT_CARRIER_LOGICAL_STATE_KEYS,
        [
            "entries",
            "package_lock_digest",
            "package_pins",
            "policy",
            "schema_version",
            "sources",
            "workspace_id",
        ]
    );

    let mut binding = carrier();
    binding.carrier_schema_id = "leanctx.context-checkpoint/v1".to_owned();
    assert!(binding.validate().is_err());

    let mut binding = carrier();
    binding.logical_state_schema_id = "leanctx.workspace.state/v2".to_owned();
    assert!(binding.validate().is_err());

    // The carrier verifier requires a canonical UUID workspace identity.
    let mut binding = carrier();
    binding.workspace_id = WorkspaceId::new("workspace-a").expect("bounded id");
    assert!(binding.validate().is_err());

    // A checkpoint that never travels in the carrier stays valid.
    let mut checkpoint = fixture();
    checkpoint.carrier = None;
    assert!(checkpoint.validate().is_ok());
}
