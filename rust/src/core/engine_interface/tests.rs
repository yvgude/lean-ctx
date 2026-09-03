use super::*;
use crate::core::data_dir;

fn digest(value: &[u8]) -> Sha256Digest {
    sha256_digest(value).expect("test SHA-256 digest")
}

fn request(
    path: &str,
    input: &[u8],
    decision: EnginePolicyDecisionV1,
) -> NativeContextEngineRequest {
    let input_ref = ProtocolReference::new("source:fixture-document").expect("input reference");
    NativeContextEngineRequest {
        invocation_id: EngineInvocationIdV1::new("engine-invocation-fixture")
            .expect("invocation id"),
        input_ref: input_ref.clone(),
        input_digest: digest(input),
        source_refs: vec![
            input_ref,
            ProtocolReference::new("source:fixture-lineage").expect("source reference"),
        ],
        policy_admission: EnginePolicyAdmissionV1 {
            policy_ref: ProtocolReference::new("policy:local-default").expect("policy reference"),
            decision,
        },
        paths: vec![path.to_owned()],
        mode: "raw".to_owned(),
        budget_tokens: None,
        timeout_ms: 0,
    }
}

fn receipt_fixture(input: &[u8]) -> (EngineInvocationV1, EngineObservationV1, Sha256Digest) {
    let root = tempfile::tempdir().expect("native adapter root");
    std::fs::write(root.path().join("fixture.md"), input).expect("fixture write");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");
    let (invocation, mut observation) = engine
        .execute(request(
            "fixture.md",
            input,
            EnginePolicyDecisionV1::Admitted,
        ))
        .expect("native Engine invocation");
    let digest = observation
        .receipt_link
        .take()
        .expect("receipt link")
        .receipt_digest;
    (invocation, observation, digest)
}

fn persist_raw_receipt(bytes: &[u8]) -> Sha256Digest {
    let digest = digest(bytes);
    persist_engine_artifact_content(RECEIPT_DIRECTORY, digest.hex(), "json", bytes)
        .expect("raw receipt artifact");
    digest
}

#[test]
fn rejected_receipt_matches_the_versioned_golden_fixture() {
    let input_ref = ProtocolReference::new("input:fixture").expect("input ref");
    let source_ref = ProtocolReference::new("source:fixture").expect("source ref");
    let invocation = EngineInvocationV1 {
        schema_version: V1_SCHEMA_VERSION,
        invocation_id: EngineInvocationIdV1::new("engine-invocation-fixture-v1")
            .expect("invocation id"),
        engine: ResolvedLocalEngineIdentityV1 {
            engine_id: ENGINE_ID.to_owned(),
            engine_version: SemanticVersion::new("1.0.0").expect("engine version"),
        },
        operation: native_operation().expect("operation"),
        input_ref: input_ref.clone(),
        input_digest: Sha256Digest::new(format!("sha256:{}", "0".repeat(64)))
            .expect("input digest"),
        source_refs: vec![input_ref, source_ref],
        policy_admission: EnginePolicyAdmissionV1 {
            policy_ref: ProtocolReference::new("policy:fixture").expect("policy ref"),
            decision: EnginePolicyDecisionV1::Rejected,
        },
    };
    let observation = EngineObservationV1 {
        schema_version: V1_SCHEMA_VERSION,
        invocation_id: invocation.invocation_id.clone(),
        status: EngineObservationStatusV1::Rejected,
        output_ref: None,
        output_digest: None,
        source_lineage: invocation.source_refs.clone(),
        measurements: Vec::new(),
        failure: Some(EngineFailureV1 {
            code: EngineFailureCodeV1::PolicyRejected,
            retryable_by_host: false,
            recovery_ref: None,
        }),
        receipt_link: None,
    };
    let artifact = EngineReceiptArtifactV1 {
        schema_version: V1_SCHEMA_VERSION,
        invocation,
        observation,
    };
    let fixture: serde_json::Value = serde_json::from_slice(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/contracts/engine-interface/v1/rejected-receipt.json"
    )))
    .expect("golden receipt fixture");

    assert_eq!(
        canonical::canonical_serialize(&artifact),
        canonical::canonical_serialize(&fixture)
    );
}

#[test]
fn verified_engine_receipt_round_trip_returns_bound_token() {
    let _data_dir = data_dir::isolated_data_dir();
    let (invocation, observation, digest) = receipt_fixture(b"verified receipt input");

    let verified = read_verified_engine_receipt(&digest, &invocation, &observation)
        .expect("receipt should verify");

    assert_eq!(verified.invocation(), &invocation);
    assert_eq!(verified.observation(), &observation);
    assert_eq!(verified.digest(), &digest);
}

#[test]
fn verified_engine_receipt_rejects_mixed_records_and_receipt_link_expectations() {
    let _data_dir = data_dir::isolated_data_dir();
    let (first_invocation, _, _) = receipt_fixture(b"first receipt input");
    let (second_invocation, mut second_observation, second_digest) =
        receipt_fixture(b"second receipt input");

    assert_ne!(
        first_invocation.input_digest,
        second_invocation.input_digest
    );
    assert!(
        read_verified_engine_receipt(&second_digest, &first_invocation, &second_observation)
            .is_err(),
        "mixed invocation and observation must fail exact binding"
    );

    second_observation.receipt_link = Some(EngineReceiptLinkV1 {
        schema_version: V1_SCHEMA_VERSION,
        receipt_id: ReceiptId::new("engine-receipt-test-link").expect("receipt id"),
        receipt_ref: ProtocolReference::new("receipt:sha256:test").expect("receipt ref"),
        receipt_digest: second_digest.clone(),
        invocation_id: second_invocation.invocation_id.clone(),
    });
    assert!(
        read_verified_engine_receipt(&second_digest, &second_invocation, &second_observation)
            .is_err(),
        "expected observation must omit receipt_link"
    );
}

#[test]
fn verified_engine_receipt_rejects_noncanonical_duplicate_unknown_and_trailing_json() {
    let _data_dir = data_dir::isolated_data_dir();
    let (invocation, observation, _) = receipt_fixture(b"strict receipt input");
    let canonical = canonical_engine_receipt_artifact_bytes(&invocation, &observation);

    let mut noncanonical = Vec::with_capacity(canonical.len() + 1);
    noncanonical.push(b' ');
    noncanonical.extend_from_slice(&canonical);
    let digest = persist_raw_receipt(&noncanonical);
    assert!(
        read_verified_engine_receipt(&digest, &invocation, &observation).is_err(),
        "leading whitespace must fail canonical-byte equality"
    );

    let canonical_text = String::from_utf8(canonical.clone()).expect("canonical JSON");
    let duplicate = canonical_text.replacen('{', "{\"schema_version\":1,", 1);
    let digest = persist_raw_receipt(duplicate.as_bytes());
    assert!(
        read_verified_engine_receipt(&digest, &invocation, &observation).is_err(),
        "duplicate fields must fail canonical-byte equality"
    );

    let mut unknown = canonical.clone();
    let end = unknown.len() - 1;
    unknown.splice(end..end, b",\"unknown\":true".iter().copied());
    let digest = persist_raw_receipt(&unknown);
    assert!(
        read_verified_engine_receipt(&digest, &invocation, &observation).is_err(),
        "unknown fields must be denied"
    );

    let mut trailing = canonical;
    trailing.extend_from_slice(b"{}");
    let digest = persist_raw_receipt(&trailing);
    assert!(
        read_verified_engine_receipt(&digest, &invocation, &observation).is_err(),
        "trailing JSON values must be denied"
    );
}

#[test]
fn verified_engine_receipt_rejects_tampering_wrong_digest_and_path_prefixes() {
    let _data_dir = data_dir::isolated_data_dir();
    let (invocation, observation, digest) = receipt_fixture(b"tamper-resistant input");
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let receipt_path = data_dir
        .join(RECEIPT_DIRECTORY)
        .join(format!("{}.json", digest.hex()));
    let mut tampered = std::fs::read(&receipt_path).expect("stored receipt");
    tampered[0] = if tampered[0] == b'{' { b'[' } else { b'{' };
    std::fs::write(&receipt_path, &tampered).expect("tamper receipt");
    assert!(
        read_verified_engine_receipt(&digest, &invocation, &observation).is_err(),
        "tampered bytes must fail the requested digest check"
    );

    let wrong_digest =
        Sha256Digest::new(format!("sha256:{}", "0".repeat(64))).expect("wrong digest");
    assert!(
        read_verified_engine_receipt(&wrong_digest, &invocation, &observation).is_err(),
        "wrong digest must not resolve a different artifact"
    );
    assert!(
        artifact_store::read_content(RECEIPT_DIRECTORY, "../receipts", "json").is_err(),
        "path prefixes must not escape the artifact namespace"
    );
}

#[cfg(unix)]
#[test]
fn verified_engine_receipt_rejects_symlinked_leaf() {
    use std::os::unix::fs::symlink;

    let _data_dir = data_dir::isolated_data_dir();
    let (invocation, observation, digest) = receipt_fixture(b"symlink-resistant input");
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let receipt_path = data_dir
        .join(RECEIPT_DIRECTORY)
        .join(format!("{}.json", digest.hex()));
    let outside = tempfile::NamedTempFile::new().expect("outside artifact");
    std::fs::write(
        outside.path(),
        canonical_engine_receipt_artifact_bytes(&invocation, &observation),
    )
    .expect("outside receipt");
    std::fs::remove_file(&receipt_path).expect("remove receipt");
    symlink(outside.path(), &receipt_path).expect("symlink receipt");

    assert!(
        read_verified_engine_receipt(&digest, &invocation, &observation).is_err(),
        "symlinked receipt leaf must be rejected"
    );
}

#[test]
fn admitted_native_operation_persists_integrity_addressed_output_and_receipt() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    let input = b"stable native context";
    std::fs::write(root.path().join("fixture.md"), input).expect("fixture write");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");

    let (invocation, observation) = engine
        .execute(request(
            "fixture.md",
            input,
            EnginePolicyDecisionV1::Admitted,
        ))
        .expect("native Engine invocation");

    assert_eq!(invocation.engine.engine_id, ENGINE_ID);
    assert_eq!(invocation.operation.capability_id.as_str(), CAPABILITY_ID);
    assert_eq!(observation.status, EngineObservationStatusV1::Succeeded);
    observation
        .validate_for(&invocation)
        .expect("Engine observation linkage");
    let output_digest = observation.output_digest.as_ref().expect("output digest");
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let output = data_dir
        .join(OUTPUT_DIRECTORY)
        .join(format!("{}.txt", output_digest.hex()));
    assert_eq!(std::fs::read(&output).expect("stored output"), input);

    let receipt = observation.receipt_link.as_ref().expect("receipt link");
    let receipt_path = data_dir
        .join(RECEIPT_DIRECTORY)
        .join(format!("{}.json", receipt.receipt_digest.hex()));
    let receipt_bytes = std::fs::read(receipt_path).expect("stored receipt");
    assert_eq!(digest(&receipt_bytes), receipt.receipt_digest);
    assert!(!String::from_utf8_lossy(&receipt_bytes).contains("stable native context"));
}

#[test]
fn repeated_native_invocation_keeps_deterministic_identity_and_output() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    let input = b"stable native context";
    std::fs::write(root.path().join("fixture.md"), input).expect("fixture write");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");
    let request = request("fixture.md", input, EnginePolicyDecisionV1::Admitted);

    let (first_invocation, first) = engine.execute(request.clone()).expect("first invocation");
    let (second_invocation, second) = engine.execute(request).expect("second invocation");

    assert_eq!(first_invocation.engine, second_invocation.engine);
    assert_eq!(first_invocation.operation, second_invocation.operation);
    assert_eq!(
        first_invocation.input_digest,
        second_invocation.input_digest
    );
    assert_eq!(first_invocation.source_refs, second_invocation.source_refs);
    assert_eq!(first.output_digest, second.output_digest);
    assert_eq!(first.receipt_link, second.receipt_link);
}

#[test]
fn materialized_execution_binds_receipt_to_the_callers_exact_snapshot() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    std::fs::write(root.path().join("fixture.md"), "different disk bytes").expect("fixture write");
    std::fs::create_dir(root.path().join("alias-parent")).expect("alias directory");
    let input = "caller snapshot\nwith stable context";
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");
    let policy_ref = "policy:ctx-read-context-gate-v1:fixture";
    let policy_admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new(policy_ref).expect("policy ref"),
        decision: EnginePolicyDecisionV1::Admitted,
    };
    let source_path = root.path().join("fixture.md");
    let (request, prepared_input) = NativeContextEngineRequest::ctx_read_snapshot(
        &source_path.to_string_lossy(),
        input,
        30_000,
        policy_admission.clone(),
    )
    .expect("production request");
    let (alias_request, alias_prepared_input) = NativeContextEngineRequest::ctx_read_snapshot(
        &root
            .path()
            .join("alias-parent/../fixture.md")
            .to_string_lossy(),
        input,
        30_000,
        policy_admission,
    )
    .expect("canonical alias request");
    assert_eq!(request.invocation_id, alias_request.invocation_id);
    assert_eq!(request.source_refs, alias_request.source_refs);
    assert_eq!(prepared_input, alias_prepared_input);
    assert!(
        request
            .input_ref
            .as_str()
            .starts_with("input:ctx-read-snapshot-sha256:")
    );

    let (invocation, observation) = engine
        .execute_materialized(request, &prepared_input)
        .expect("materialized Engine invocation");

    assert_eq!(invocation.input_digest, digest(prepared_input.as_bytes()));
    assert_eq!(invocation.policy_admission.policy_ref.as_str(), policy_ref);
    assert_eq!(observation.status, EngineObservationStatusV1::Succeeded);
    assert!(observation.receipt_link.is_some());
    assert!(
        observation
            .measurements
            .iter()
            .all(|measurement| measurement.name != "latency_ms")
    );
}

#[test]
fn production_snapshot_refuses_a_source_outside_the_engine_root() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    let outside = tempfile::tempdir().expect("outside root");
    let source = outside.path().join("escape.md");
    std::fs::write(&source, "outside").expect("outside fixture");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");
    let admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new("policy:ctx-read-context-gate-v1:fixture")
            .expect("policy ref"),
        decision: EnginePolicyDecisionV1::Admitted,
    };

    let error = engine
        .execute_ctx_read_snapshot(&source.to_string_lossy(), "outside", admission)
        .unwrap_err();
    assert_eq!(
        error,
        "ctx_read Engine source is outside its rooted boundary"
    );
}

#[test]
fn materialized_execution_enforces_a_real_host_deadline() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    let source = root.path().join("fixture.md");
    std::fs::write(&source, "deadline fixture").expect("fixture write");
    let control = std::sync::Arc::new(
        crate::core::ocla::adapters::native_context::MaterializedTestControl::new(),
    );
    let engine = NativeContextEngine {
        adapter: NativeContextAdapter::with_root(root.path())
            .with_materialized_test_control(control.clone()),
    };
    let admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new("policy:ctx-read-context-gate-v1:fixture")
            .expect("policy ref"),
        decision: EnginePolicyDecisionV1::Admitted,
    };
    let (request, input) = NativeContextEngineRequest::ctx_read_snapshot(
        &source.to_string_lossy(),
        "deadline fixture",
        25,
        admission,
    )
    .expect("bounded request");

    let started = std::time::Instant::now();
    let (_, observation) = engine
        .execute_materialized(request, &input)
        .expect("deadline failure receipt");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(observation.status, EngineObservationStatusV1::Failed);
    assert_eq!(
        observation.failure.as_ref().expect("deadline failure").code,
        EngineFailureCodeV1::ResourceLimit
    );
    assert!(observation.receipt_link.is_some());
    let output_dir = data_dir::lean_ctx_data_dir()
        .expect("isolated data dir")
        .join(OUTPUT_DIRECTORY);
    assert!(!output_dir.exists());
    control.release.wait();
    control.completed.wait();
    assert!(!output_dir.exists());
}

#[cfg(unix)]
#[test]
fn existing_engine_artifact_permissions_are_rehardened() {
    use std::os::unix::fs::PermissionsExt;

    let _data_dir = data_dir::isolated_data_dir();
    let bytes = b"permission fixture";
    let digest = digest(bytes);
    persist_output(digest.hex(), bytes).expect("initial artifact");
    let path = data_dir::lean_ctx_data_dir()
        .expect("isolated data dir")
        .join(OUTPUT_DIRECTORY)
        .join(format!("{}.txt", digest.hex()));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("loosen fixture permissions");

    persist_output(digest.hex(), bytes).expect("reharden existing artifact");
    assert_eq!(
        std::fs::metadata(path)
            .expect("artifact metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn existing_engine_artifact_rejects_tampering_and_symlinks() {
    use std::os::unix::fs::symlink;

    let _data_dir = data_dir::isolated_data_dir();
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let output_dir = data_dir.join(OUTPUT_DIRECTORY);
    std::fs::create_dir_all(&output_dir).expect("output directory");
    let bytes = b"expected output";
    let digest = digest(bytes);
    let path = output_dir.join(format!("{}.txt", digest.hex()));
    std::fs::write(&path, b"tampered output").expect("tampered artifact");
    assert!(
        persist_output(digest.hex(), bytes)
            .unwrap_err()
            .contains("digest")
    );

    std::fs::remove_file(&path).expect("remove tampered artifact");
    let target = output_dir.join("target.txt");
    std::fs::write(&target, bytes).expect("symlink target");
    symlink(&target, &path).expect("artifact symlink");
    assert!(
        persist_output(digest.hex(), bytes)
            .unwrap_err()
            .contains("engine_artifact_leaf_untrusted")
    );
}

#[cfg(unix)]
#[test]
fn symlinked_engine_artifact_directory_is_rejected_before_any_write() {
    use std::os::unix::fs::symlink;

    let _data_dir = data_dir::isolated_data_dir();
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let engine_dir = data_dir.join("engine-interface/v1");
    std::fs::create_dir_all(&engine_dir).expect("Engine directory");
    let outside = tempfile::tempdir().expect("outside directory");
    symlink(outside.path(), engine_dir.join("outputs")).expect("artifact directory symlink");
    let bytes = b"must remain inside the Engine data root";
    let digest = digest(bytes);

    let error = persist_output(digest.hex(), bytes).expect_err("symlinked directory rejected");

    assert!(error.contains("engine_artifact_boundary_rejected"));
    assert_eq!(
        std::fs::read_dir(outside.path())
            .expect("outside directory")
            .count(),
        0
    );
}

#[cfg(any(unix, windows))]
#[test]
fn descriptor_relative_parent_swap_never_writes_replacement_or_outside() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    let _data_dir = data_dir::isolated_data_dir();
    let data_root = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let output_dir = data_root.join(OUTPUT_DIRECTORY);
    let opened_dir = data_root.join("engine-interface/v1/outputs.opened");
    let outside = tempfile::tempdir().expect("outside directory");
    let outside_path = outside.path().to_path_buf();
    let sentinel = outside_path.join("sentinel.txt");
    std::fs::write(&sentinel, b"OUTSIDE_SENTINEL_V1").expect("outside sentinel");
    let bytes = b"descriptor-relative artifact";
    let digest = digest(bytes);
    let final_name = format!("{}.txt", digest.hex());

    let barrier_output_dir = output_dir.clone();
    let barrier_opened_dir = opened_dir.clone();
    let barrier_outside_path = outside_path.clone();
    let barrier = Box::new(move || {
        std::fs::rename(&barrier_output_dir, &barrier_opened_dir).expect("rename opened directory");
        #[cfg(unix)]
        symlink(&barrier_outside_path, &barrier_output_dir).expect("replacement symlink");
        #[cfg(windows)]
        {
            let _ = barrier_outside_path;
            std::fs::create_dir(&barrier_output_dir).expect("replacement directory");
        }
    });
    let result = artifact_store::persist_content_with_test_barrier(
        OUTPUT_DIRECTORY,
        digest.hex(),
        "txt",
        bytes,
        barrier,
    );

    assert_eq!(
        std::fs::read(&sentinel)
            .expect("outside sentinel remains")
            .as_slice(),
        b"OUTSIDE_SENTINEL_V1"
    );
    assert_eq!(
        std::fs::read_dir(&outside_path)
            .expect("outside directory")
            .count(),
        1,
        "replacement/outside received no artifact or temporary leaf"
    );
    #[cfg(windows)]
    assert_eq!(
        std::fs::read_dir(&output_dir)
            .expect("replacement directory")
            .count(),
        0,
        "replacement directory received no artifact or temporary leaf"
    );
    match result {
        Ok(_) => {
            assert_eq!(
                std::fs::read(opened_dir.join(final_name)).expect("held directory artifact"),
                bytes
            );
            assert_eq!(
                std::fs::read_dir(opened_dir)
                    .expect("held directory")
                    .count(),
                1,
                "held directory contains only the published artifact"
            );
        }
        Err(error) => {
            assert!(error.starts_with("engine_artifact_"));
            assert!(!error.contains("errno"));
            assert!(
                std::fs::read_dir(opened_dir)
                    .expect("held directory")
                    .flatten()
                    .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp")),
                "failed publication leaves no temporary leaf"
            );
        }
    }
}

/// Entries in a replacement data root that mean the bound write was re-resolved
/// by path — the breach the relocation guard exists to catch (#1658).
///
/// Directories are bound before the root is renamed away, so nothing can reach
/// the replacement except through a fresh path resolution, which would recreate
/// the artifact tree, publish the final name, or leave its temporary. Anything
/// else in there came from a different writer and is not this guard's subject;
/// failing on it is what made the guard flaky without ever naming a cause.
fn boundary_breach<'a>(entries: &'a [String], final_name: &str) -> Vec<&'a String> {
    let artifact_tree = OUTPUT_DIRECTORY
        .split('/')
        .next()
        .expect("output directory has a first component");
    entries
        .iter()
        .filter(|name| {
            name.as_str() == artifact_tree || name.as_str() == final_name || name.contains(".tmp")
        })
        .collect()
}

/// The relaxed guard must still catch what it was written for. Both directions
/// are pinned here so a later relaxation cannot quietly mute the invariant.
#[test]
fn the_relocation_guard_catches_a_breach_and_ignores_a_foreign_writer() {
    let final_name = "f00d.txt".to_string();

    for breach in [
        "engine-interface".to_string(),
        final_name.clone(),
        "f00d.txt.tmp91237".to_string(),
    ] {
        let entries = vec![breach.clone()];
        assert_eq!(
            boundary_breach(&entries, &final_name).len(),
            1,
            "must still be caught: {breach}"
        );
    }

    let foreign = vec![
        "archives".to_string(),
        "sessions".to_string(),
        "cloud".to_string(),
        "knowledge.db".to_string(),
    ];
    assert!(
        boundary_breach(&foreign, &final_name).is_empty(),
        "an unrelated writer is not a descriptor-binding breach"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn descriptor_bound_root_relocation_never_retargets_a_replacement_root() {
    let _data_dir = data_dir::isolated_data_dir();
    let data_root = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let opened_root = data_root.with_extension("opened");
    let bytes = b"descriptor-bound root artifact";
    let digest = digest(bytes);
    let final_name = format!("{}.txt", digest.hex());

    let relocated = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let barrier_data_root = data_root.clone();
    let barrier_opened_root = opened_root.clone();
    let barrier_relocated = std::sync::Arc::clone(&relocated);
    let barrier =
        Box::new(
            move || match std::fs::rename(&barrier_data_root, &barrier_opened_root) {
                Ok(()) => {
                    std::fs::create_dir(&barrier_data_root).expect("replacement data root");
                    barrier_relocated.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Err(error) => {
                    #[cfg(windows)]
                    assert_eq!(
                        error.kind(),
                        std::io::ErrorKind::PermissionDenied,
                        "Windows may only reject relocation while held handles are open"
                    );
                    #[cfg(unix)]
                    panic!("relocate bound data root: {error}");
                }
            },
        );
    let result = artifact_store::persist_content_with_test_barrier(
        OUTPUT_DIRECTORY,
        digest.hex(),
        "txt",
        bytes,
        barrier,
    );

    let did_relocate = relocated.load(std::sync::atomic::Ordering::SeqCst);
    let bound_root = if did_relocate {
        // #1658: assert the invariant, not emptiness. This used to require the
        // replacement root to have *no entries at all*, while its own message
        // claimed the narrower "no artifact or temporary leaf" — so any
        // unrelated writer touching the data directory failed a test about
        // descriptor binding, intermittently and only on Linux. `left: 1,
        // right: 0` named nothing, which is why the first failure could not be
        // told apart from a real boundary breach.
        //
        // What a real breach looks like is specific: the directories are bound
        // *before* the barrier renames the root, so anything landing in the
        // replacement can only have got there by re-resolving the path
        // afterwards — which would recreate the artifact tree
        // (`engine-interface/…`), the published name, or its temporary leaf.
        // Those still fail, and now they say what was found.
        let entries: Vec<String> = std::fs::read_dir(&data_root)
            .expect("replacement data root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        let breach = boundary_breach(&entries, &final_name);
        assert!(
            breach.is_empty(),
            "replacement root received an artifact or temporary leaf: {breach:?} \
             (all entries: {entries:?})"
        );
        &opened_root
    } else {
        #[cfg(unix)]
        {
            panic!("Unix relocation must succeed");
        }
        #[cfg(windows)]
        {
            assert!(
                !opened_root.exists(),
                "denied relocation must not create a partial target"
            );
            &data_root
        }
    };
    match result {
        Ok(_) => assert_eq!(
            std::fs::read(bound_root.join(OUTPUT_DIRECTORY).join(final_name))
                .expect("artifact remains under bound root object"),
            bytes
        ),
        Err(error) => {
            assert!(error.starts_with("engine_artifact_"));
            assert!(
                std::fs::read_dir(bound_root.join(OUTPUT_DIRECTORY))
                    .expect("bound output directory")
                    .flatten()
                    .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp")),
                "failed publication leaves no known temporary leaf"
            );
        }
    }
    if did_relocate {
        std::fs::remove_dir_all(&opened_root).expect("remove relocated test root");
    }
}

#[cfg(unix)]
#[test]
fn swapped_unix_temp_leaf_is_rejected_and_never_published() {
    use std::os::unix::fs::symlink;

    let _data_dir = data_dir::isolated_data_dir();
    let data_root = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let output_dir = data_root.join(OUTPUT_DIRECTORY);
    let outside = tempfile::tempdir().expect("outside directory");
    let sentinel = outside.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"OUTSIDE_SENTINEL_V1").expect("outside sentinel");
    let bytes = b"held temporary artifact";
    let digest = digest(bytes);
    let temp_path = output_dir.join(format!(".{}.txt.tmp", digest.hex()));
    let final_path = output_dir.join(format!("{}.txt", digest.hex()));

    let barrier_temp_path = temp_path.clone();
    let barrier_sentinel = sentinel.clone();
    let barrier = Box::new(move || {
        std::fs::remove_file(&barrier_temp_path).expect("unlink held temporary name");
        symlink(&barrier_sentinel, &barrier_temp_path).expect("swap temporary name");
    });
    let error = artifact_store::persist_content_with_test_publish_barrier(
        OUTPUT_DIRECTORY,
        digest.hex(),
        "txt",
        bytes,
        barrier,
    )
    .expect_err("swapped temporary leaf rejected");

    assert_eq!(error, "engine_artifact_leaf_untrusted");
    assert!(!final_path.exists());
    assert!(!temp_path.exists());
    assert_eq!(
        std::fs::read(&sentinel).expect("outside sentinel remains"),
        b"OUTSIDE_SENTINEL_V1"
    );
}

#[cfg(windows)]
#[test]
fn oversized_windows_artifact_component_is_rejected_before_child_mutation() {
    let _data_dir = data_dir::isolated_data_dir();
    let bytes = b"bounded Windows component";
    let digest = digest(bytes);
    let oversized = "x".repeat((u16::MAX as usize / 2) + 1);

    let error = artifact_store::persist_content(&oversized, digest.hex(), "txt", bytes)
        .expect_err("oversized component rejected");

    assert_eq!(error, "engine_artifact_boundary_rejected");
    let data_root = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    assert_eq!(std::fs::read_dir(data_root).expect("data root").count(), 0);
}

#[test]
fn failed_engine_artifact_publish_leaves_final_absent_and_retryable() {
    let _data_dir = data_dir::isolated_data_dir();
    let bytes = b"failure-atomic artifact fixture";
    let digest = digest(bytes);
    let output_dir = data_dir::lean_ctx_data_dir()
        .expect("isolated data dir")
        .join(OUTPUT_DIRECTORY);
    let final_path = output_dir.join(format!("{}.txt", digest.hex()));

    artifact_store::inject_test_pre_publish_failure();
    assert_eq!(
        persist_output(digest.hex(), bytes).expect_err("injected publish failure"),
        "engine_artifact_test_pre_publish_failure"
    );
    assert!(
        !final_path.exists(),
        "failed publish must not expose final path"
    );
    assert_eq!(
        std::fs::read_dir(&output_dir)
            .expect("output directory")
            .count(),
        0,
        "failed publish must clean its temporary leaf"
    );

    persist_output(digest.hex(), bytes).expect("retry publishes complete artifact");
    assert_eq!(
        std::fs::read(&final_path).expect("published artifact"),
        bytes
    );
    assert_eq!(
        std::fs::read_dir(&output_dir)
            .expect("output directory")
            .count(),
        1,
        "successful retry leaves only the addressed artifact"
    );
}

#[cfg(windows)]
#[test]
fn failed_windows_temp_validation_cleans_provisional_leaf_and_is_retryable() {
    let _data_dir = data_dir::isolated_data_dir();
    let bytes = b"Windows temp validation fixture";
    let digest = digest(bytes);
    let output_dir = data_dir::lean_ctx_data_dir()
        .expect("isolated data dir")
        .join(OUTPUT_DIRECTORY);
    let final_path = output_dir.join(format!("{}.txt", digest.hex()));

    artifact_store::inject_test_temp_validation_failure();
    assert_eq!(
        persist_output(digest.hex(), bytes).expect_err("injected validation failure"),
        "engine_artifact_leaf_untrusted"
    );
    assert_eq!(
        std::fs::read_dir(&output_dir)
            .expect("output directory")
            .count(),
        0,
        "failed validation must delete its provisional leaf"
    );

    persist_output(digest.hex(), bytes).expect("retry publishes complete artifact");
    assert_eq!(
        std::fs::read(&final_path).expect("published artifact after retry"),
        bytes
    );
}

#[test]
fn ctx_read_identity_is_bound_to_raw_snapshot_bytes() {
    let root = tempfile::tempdir().expect("native adapter root");
    let path = root.path().join("fixture.md");
    std::fs::write(&path, "fixture").expect("fixture write");
    let admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new("policy:ctx-read-context-gate-v1:fixture")
            .expect("policy ref"),
        decision: EnginePolicyDecisionV1::Admitted,
    };
    let (first, _) = NativeContextEngineRequest::ctx_read_snapshot(
        &path.to_string_lossy(),
        "raw snapshot A",
        30_000,
        admission.clone(),
    )
    .expect("first request");
    let (second, _) = NativeContextEngineRequest::ctx_read_snapshot(
        &path.to_string_lossy(),
        "raw snapshot B",
        30_000,
        admission,
    )
    .expect("second request");

    assert_ne!(first.input_ref, second.input_ref);
    assert_ne!(first.invocation_id, second.invocation_id);
}

#[test]
fn production_snapshot_redacts_secret_before_output_and_recovery_persistence() {
    let _data_dir = data_dir::isolated_data_dir();
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let engine_dir = data_dir.join("engine-interface/v1");
    std::fs::create_dir_all(&engine_dir).expect("Engine directory");
    std::fs::write(engine_dir.join("receipts"), "blocks receipt directory")
        .expect("receipt blocker");
    let root = tempfile::tempdir().expect("native adapter root");
    let path = root.path().join("secret.md");
    std::fs::write(&path, "source placeholder").expect("fixture write");
    let secret = format!(
        "api_key={}",
        ["not", "-a-real-secret-", "1234567890abcdef"].concat()
    );
    let admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new("policy:ctx-read-context-gate-v1:redaction")
            .expect("policy ref"),
        decision: EnginePolicyDecisionV1::Admitted,
    };

    let error = NativeContextEngine::with_root(root.path())
        .expect("secure Engine root")
        .execute_ctx_read_snapshot(&path.to_string_lossy(), &secret, admission)
        .expect_err("blocked receipt must return a durable recovery error");
    assert!(!error.contains(&secret));

    for directory in [OUTPUT_DIRECTORY, RECOVERY_DIRECTORY] {
        for entry in std::fs::read_dir(data_dir.join(directory)).expect("artifact directory") {
            let bytes =
                std::fs::read(entry.expect("artifact entry").path()).expect("artifact bytes");
            assert!(!String::from_utf8_lossy(&bytes).contains(&secret));
        }
    }
}

#[test]
fn rejected_policy_never_attempts_the_missing_source() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");

    let (_, observation) = engine
        .execute(request(
            "missing.md",
            b"unread input candidate",
            EnginePolicyDecisionV1::Rejected,
        ))
        .expect("policy rejection record");

    assert_eq!(observation.status, EngineObservationStatusV1::Rejected);
    assert_eq!(
        observation.failure.expect("failure record").code,
        EngineFailureCodeV1::PolicyRejected
    );
}

#[test]
fn missing_source_has_structured_recovery_route() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");

    let (_, observation) = engine
        .execute(request(
            "missing.md",
            b"expected source bytes",
            EnginePolicyDecisionV1::Admitted,
        ))
        .expect("source failure record");

    let failure = observation.failure.expect("failure record");
    assert_eq!(failure.code, EngineFailureCodeV1::SourceUnavailable);
    assert!(failure.recovery_ref.is_some());
}

#[test]
fn source_integrity_mismatch_is_explicit() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("native adapter root");
    std::fs::write(root.path().join("fixture.md"), b"actual source").expect("fixture write");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");

    let (_, observation) = engine
        .execute(request(
            "fixture.md",
            b"different expected source",
            EnginePolicyDecisionV1::Admitted,
        ))
        .expect("integrity failure record");

    let failure = observation.failure.expect("failure record");
    assert_eq!(failure.code, EngineFailureCodeV1::SourceIntegrityMismatch);
    assert!(failure.recovery_ref.is_some());
}

#[test]
fn output_persistence_failure_is_receipted_and_retryable() {
    let _data_dir = data_dir::isolated_data_dir();
    let data_dir = data_dir::lean_ctx_data_dir().expect("isolated data dir");
    let engine_dir = data_dir.join("engine-interface/v1");
    std::fs::create_dir_all(&engine_dir).expect("Engine directory");
    std::fs::write(engine_dir.join("outputs"), "blocks output directory").expect("output blocker");
    let root = tempfile::tempdir().expect("native adapter root");
    let input = b"retryable native context";
    std::fs::write(root.path().join("fixture.md"), input).expect("fixture write");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");
    let request = request("fixture.md", input, EnginePolicyDecisionV1::Admitted);

    let (_, failed) = engine.execute(request.clone()).expect("failed receipt");
    assert_eq!(failed.status, EngineObservationStatusV1::Failed);
    assert!(failed.receipt_link.is_some());
    let failure = failed.failure.expect("failure record");
    assert_eq!(failure.code, EngineFailureCodeV1::Internal);
    assert!(failure.retryable_by_host);

    std::fs::remove_file(engine_dir.join("outputs")).expect("remove output blocker");
    std::fs::create_dir(engine_dir.join("outputs")).expect("output directory");
    let (_, retried) = engine.execute(request).expect("successful retry");
    assert_eq!(retried.status, EngineObservationStatusV1::Succeeded);
    assert!(retried.receipt_link.is_some());
}

#[test]
fn engine_root_binding_never_falls_back_to_an_unresolved_path() {
    let parent = tempfile::tempdir().expect("root parent");
    let missing = parent.path().join("missing-root");

    let error = NativeContextEngine::with_root(&missing)
        .err()
        .expect("missing Engine root rejected");

    assert_eq!(error, "ctx_read Engine root cannot be bound securely");
    assert!(!missing.exists());
}

#[test]
fn interface_matches_the_native_capability_contract() {
    let root = tempfile::tempdir().expect("native adapter root");
    let engine = NativeContextEngine::with_root(root.path()).expect("secure Engine root");
    let interface = engine.interface().expect("Engine interface");
    assert_eq!(interface.engine.engine_id, ENGINE_ID);
    assert_eq!(interface.supported_operations.len(), 1);
    assert_eq!(
        interface.supported_operations[0].capability_id.as_str(),
        CAPABILITY_ID
    );
}

#[test]
fn transport_context_view_and_recovery_are_deterministic_and_exact() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("transport root");
    let source = root.path().join("fixture.rs");
    let input = "fn fixture() { let stable = true; }\n";
    std::fs::write(&source, input).expect("fixture write");

    let first =
        execute_transport_context_view(root.path(), "fixture.rs").expect("transport context view");
    let second =
        execute_transport_context_view(root.path(), "fixture.rs").expect("deterministic repeat");
    assert_eq!(first, second);
    assert_eq!(
        first.recovery.recovery_ref,
        first.invocation.as_ref().unwrap().input_ref.clone()
    );
    assert_eq!(first.recovery.source_digest, digest(input.as_bytes()));
    assert_eq!(
        first.view.output_digest,
        first.observation.as_ref().unwrap().output_digest
    );
    first
        .observation
        .as_ref()
        .unwrap()
        .validate_for(first.invocation.as_ref().unwrap())
        .expect("transport records validate");

    let recovered = recover_transport_source(
        root.path(),
        "fixture.rs",
        &first.recovery.recovery_ref,
        &first.recovery.source_ref,
        &first.recovery.source_digest,
    )
    .expect("exact recovery");
    assert_eq!(recovered.view.text, input);
    assert_eq!(
        recovered.view.output_digest,
        Some(first.recovery.source_digest.clone())
    );
}

#[test]
fn transport_recovery_fails_closed_for_malformed_changed_missing_and_outside_sources() {
    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("transport root");
    let source = root.path().join("fixture.txt");
    std::fs::write(&source, "original").expect("fixture write");
    let result =
        execute_transport_context_view(root.path(), "fixture.txt").expect("transport context view");

    let malformed = ProtocolReference::new("recovery:not-an-input-ref").expect("reference");
    assert_eq!(
        recover_transport_source(
            root.path(),
            "fixture.txt",
            &malformed,
            &result.recovery.source_ref,
            &result.recovery.source_digest,
        )
        .expect_err("malformed recovery ref"),
        EngineTransportError::MalformedRecoveryRef
    );

    std::fs::write(&source, "changed").expect("changed source");
    assert_eq!(
        recover_transport_source(
            root.path(),
            "fixture.txt",
            &result.recovery.recovery_ref,
            &result.recovery.source_ref,
            &result.recovery.source_digest,
        )
        .expect_err("changed source"),
        EngineTransportError::SourceChanged
    );
    std::fs::remove_file(&source).expect("remove source");
    assert_eq!(
        recover_transport_source(
            root.path(),
            "fixture.txt",
            &result.recovery.recovery_ref,
            &result.recovery.source_ref,
            &result.recovery.source_digest,
        )
        .expect_err("missing source"),
        EngineTransportError::SourceUnavailable
    );

    let outside = tempfile::tempdir().expect("outside root");
    let outside_source = outside.path().join("outside.txt");
    std::fs::write(&outside_source, "outside").expect("outside fixture");
    assert_eq!(
        execute_transport_context_view(root.path(), &outside_source.to_string_lossy())
            .expect_err("outside source"),
        EngineTransportError::SourceOutsideRoot
    );
    assert_eq!(
        execute_transport_context_view(std::path::Path::new("/"), "fixture.txt")
            .expect_err("unsafe root"),
        EngineTransportError::UnsafeRoot
    );
}

#[cfg(unix)]
#[test]
fn transport_rejects_a_symlinked_source_leaf() {
    use std::os::unix::fs::symlink;

    let _data_dir = data_dir::isolated_data_dir();
    let root = tempfile::tempdir().expect("transport root");
    let target = root.path().join("target.txt");
    let link = root.path().join("link.txt");
    std::fs::write(&target, "target").expect("target fixture");
    symlink(&target, &link).expect("symlink fixture");

    assert_eq!(
        execute_transport_context_view(root.path(), "link.txt").expect_err("symlink source"),
        EngineTransportError::SourceSymlink
    );

    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");
    let nested_link = nested.join("link");
    symlink(".", &nested_link).expect("nested symlink fixture");
    assert_eq!(
        execute_transport_context_view(root.path(), "nested/link/../target.txt")
            .expect_err("symlink source component"),
        EngineTransportError::SourceSymlink
    );
}
