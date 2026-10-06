// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::audit_trail::AuditEntry;

/// A digest that is valid hex but is not the real binary/role digest.
const FOREIGN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const FOREIGN_2: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn isolated() -> crate::core::data_dir::IsolatedDataDir {
    crate::core::data_dir::isolated_data_dir()
}

fn data_dir() -> PathBuf {
    crate::core::data_dir::lean_ctx_data_dir().expect("data dir")
}

/// Rewrite the STORED attestation so the next heartbeat observes exactly
/// the requested dimensions against the real, untouched process. Nothing
/// about the running binary or the role file is faked.
fn seed_stored(agent_id: &str, binary_sha256: &str, config_sha256: &str) {
    with_registry(|reg| {
        let attestation = reg
            .get_mut(agent_id)
            .expect("registered")
            .attestation
            .as_mut()
            .expect("attested at registration");
        attestation.binary_sha256 = binary_sha256.to_string();
        attestation.config_sha256 = config_sha256.to_string();
        Ok(())
    })
    .expect("seed stored attestation");
}

fn stored(agent_id: &str) -> Attestation {
    get(agent_id)
        .expect("record")
        .attestation
        .expect("attestation")
}

fn drift_entries() -> Vec<AuditEntry> {
    audit_trail::load_recent(1024)
        .into_iter()
        .filter(|entry| matches!(entry.event_type, AuditEventType::AgentDriftDetected))
        .collect()
}

fn acknowledgement_entries() -> Vec<AuditEntry> {
    audit_trail::load_recent(1024)
        .into_iter()
        .filter(|entry| matches!(entry.event_type, AuditEventType::AgentDriftAcknowledged))
        .collect()
}

/// Register and return the genuine attestation observed for this process.
fn register_attested(agent_id: &str) -> Attestation {
    register(agent_id, "coder", "yves@org").expect("register");
    let truth = stored(agent_id);
    assert!(
        truth.config_sha256.is_empty(),
        "built-in role must have no role file in an isolated data dir"
    );
    truth
}

#[test]
fn binary_only_drift_persists_fresh_attestation_and_audits_once() {
    let _iso = isolated();
    let truth = register_attested("d-binary");
    seed_stored("d-binary", FOREIGN, &truth.config_sha256);

    let evidence = heartbeat("d-binary").expect("heartbeat").expect("drift");
    assert!(
        evidence.contains("dimensions=binary binary=111111111111->"),
        "{evidence}"
    );
    assert!(
        !evidence.contains("config="),
        "config dimension did not change: {evidence}"
    );

    let after = stored("d-binary");
    assert_eq!(
        after.binary_sha256, truth.binary_sha256,
        "freshly observed attestation must be persisted"
    );
    let mark = after.drift.expect("sticky drift mark");
    assert!(mark.binary && !mark.config, "{mark:?}");

    let entries = drift_entries();
    assert_eq!(entries.len(), 1, "exactly one drift entry: {entries:?}");
    assert!(
        evidence.starts_with(entries[0].action.as_deref().expect("audit evidence")),
        "sticky evidence must bind the audited observation: {evidence}"
    );
    assert!(evidence.contains(" observation="), "{evidence}");
    assert_eq!(entries[0].agent_id, "d-binary");

    // The next beat compares against the NEW state → no repeat event.
    assert!(heartbeat("d-binary").expect("second beat").is_none());
    assert_eq!(drift_entries().len(), 1);
    assert!(audit_trail::verify_chain().valid);
}

#[test]
fn config_only_drift_persists_fresh_attestation_and_audits_once() {
    let _iso = isolated();
    let truth = register_attested("d-config");
    seed_stored("d-config", &truth.binary_sha256, FOREIGN);

    let evidence = heartbeat("d-config").expect("heartbeat").expect("drift");
    assert!(
        evidence.contains("dimensions=config config=111111111111->none"),
        "{evidence}"
    );
    assert!(
        !evidence.contains("binary="),
        "binary dimension did not change: {evidence}"
    );

    let after = stored("d-config");
    assert_eq!(after.config_sha256, truth.config_sha256);
    let mark = after.drift.expect("sticky drift mark");
    assert!(!mark.binary && mark.config, "{mark:?}");
    assert_eq!(drift_entries().len(), 1);

    assert!(heartbeat("d-config").expect("second beat").is_none());
    assert_eq!(drift_entries().len(), 1);
}

/// The regression this slice exists for: the old `if / else if` chain
/// reported binary drift and silently dropped the config dimension.
#[test]
fn simultaneous_drift_reports_both_dimensions_and_never_masks_config() {
    let _iso = isolated();
    let truth = register_attested("d-both");
    seed_stored("d-both", FOREIGN, FOREIGN);

    let evidence = heartbeat("d-both").expect("heartbeat").expect("drift");
    assert!(evidence.contains("dimensions=binary+config"), "{evidence}");
    assert!(
        evidence.contains("binary=111111111111->")
            && evidence.contains("config=111111111111->none"),
        "both dimensions must carry their own evidence: {evidence}"
    );

    let after = stored("d-both");
    assert_eq!(after.binary_sha256, truth.binary_sha256);
    assert_eq!(after.config_sha256, truth.config_sha256);
    let mark = after.drift.expect("sticky drift mark");
    assert!(mark.binary && mark.config, "{mark:?}");
    assert_eq!(drift_entries().len(), 1);
    assert!(heartbeat("d-both").expect("second beat").is_none());
    assert_eq!(drift_entries().len(), 1);
}

#[test]
fn unchanged_heartbeat_emits_no_drift_event() {
    let _iso = isolated();
    register_attested("d-clean");
    assert!(heartbeat("d-clean").expect("beat 1").is_none());
    assert!(heartbeat("d-clean").expect("beat 2").is_none());
    assert!(drift_entries().is_empty(), "clean beats must stay silent");
    assert!(stored("d-clean").drift.is_none());
    assert!(!check("d-clean").drifted);
    assert!(check("d-clean").allowed);
}

#[test]
fn block_on_drift_denies_only_when_explicitly_enabled() {
    let _iso = isolated();
    // `isolated()` holds the test env lock, so clearing the opt-in here
    // is sound and makes the default assertion independent of the shell.
    crate::test_env::remove_var("LEAN_CTX_AGENT_BLOCK_ON_DRIFT");
    register_attested("d-policy");
    seed_stored("d-policy", FOREIGN, FOREIGN);
    let first = heartbeat("d-policy").expect("heartbeat");
    let evidence = first.evidence().expect("drift evidence").to_string();

    assert_eq!(
        DriftPolicy::default(),
        DriftPolicy {
            block_on_drift: false
        },
        "documented compatibility default"
    );
    let permissive = check("d-policy");
    assert!(permissive.drifted, "drift must still be reported");
    assert!(
        permissive.allowed,
        "compatibility default must not change the allow bit: {permissive:?}"
    );
    assert!(permissive.detail.contains("advisory"), "{permissive:?}");

    let blocking = check_with_policy(
        "d-policy",
        DriftPolicy {
            block_on_drift: true,
        },
    );
    assert!(!blocking.allowed, "{blocking:?}");
    assert!(blocking.drifted);
    assert!(
        blocking
            .detail
            .contains("attestation drift dimensions=binary+config"),
        "{blocking:?}"
    );

    // A clean beat must NOT expire the mark, or blocking would last one beat.
    let sticky = heartbeat("d-policy").expect("clean beat");
    assert!(sticky.is_none(), "no new transition on a clean beat");
    assert!(!sticky.newly_observed && sticky.drift.is_some());
    assert!(
        !check_with_policy(
            "d-policy",
            DriftPolicy {
                block_on_drift: true
            }
        )
        .allowed,
        "drift mark must be sticky until acknowledged"
    );

    // Plain resume is lifecycle-only and cannot acknowledge drift.
    resume("d-policy").expect("resume");
    assert!(stored("d-policy").drift.is_some());
    assert!(
        !check_with_policy(
            "d-policy",
            DriftPolicy {
                block_on_drift: true
            }
        )
        .allowed
    );

    // Explicit operator acknowledgement clears exactly the evidence seen
    // above; the durable audit entry names that same evidence.
    let ack = acknowledge_drift("d-policy", &evidence).expect("acknowledge");
    assert_eq!(ack.evidence, evidence);
    let acknowledged = check_with_policy(
        "d-policy",
        DriftPolicy {
            block_on_drift: true,
        },
    );
    assert!(
        acknowledged.allowed && !acknowledged.drifted,
        "{acknowledged:?}"
    );
    assert_eq!(
        drift_entries().len(),
        1,
        "acknowledging never erases evidence"
    );
    let acks = acknowledgement_entries();
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].action.as_deref(), Some(evidence.as_str()));
}

#[test]
fn legacy_attestation_without_drift_key_round_trips() {
    let legacy = serde_json::json!({
        "binary_sha256": FOREIGN,
        "config_sha256": "",
        "attested_at": "2026-09-06T20:00:00Z"
    });
    let attestation: Attestation = serde_json::from_value(legacy).expect("legacy serde");
    assert!(attestation.drift.is_none());
    let round_trip = serde_json::to_value(attestation).expect("round-trip serde");
    assert!(round_trip.get("drift").is_none());
}

#[test]
fn concurrent_heartbeats_emit_one_transition_and_keep_sticky_state() {
    use std::sync::{Arc, Barrier};

    let _iso = isolated();
    let truth = register_attested("d-concurrent-hb");
    seed_stored("d-concurrent-hb", FOREIGN, &truth.config_sha256);
    let barrier = Arc::new(Barrier::new(3));
    let handles = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                heartbeat("d-concurrent-hb").expect("concurrent heartbeat")
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("heartbeat thread"))
        .collect::<Vec<_>>();

    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.newly_observed)
            .count(),
        1,
        "registry lock must serialize the transition"
    );
    assert!(outcomes.iter().all(|outcome| outcome.drift.is_some()));
    assert_eq!(drift_entries().len(), 1);
    assert!(audit_trail::verify_chain().valid);
}

#[test]
fn concurrent_heartbeat_and_resume_never_clear_drift() {
    use std::sync::{Arc, Barrier};

    let _iso = isolated();
    let truth = register_attested("d-concurrent-resume");
    seed_stored("d-concurrent-resume", FOREIGN, &truth.config_sha256);
    let barrier = Arc::new(Barrier::new(3));
    let heartbeat_barrier = Arc::clone(&barrier);
    let heartbeat_thread = std::thread::spawn(move || {
        heartbeat_barrier.wait();
        heartbeat("d-concurrent-resume").expect("heartbeat")
    });
    let resume_barrier = Arc::clone(&barrier);
    let resume_thread = std::thread::spawn(move || {
        resume_barrier.wait();
        resume("d-concurrent-resume").expect("resume");
    });
    barrier.wait();
    let heartbeat_outcome = heartbeat_thread.join().expect("heartbeat thread");
    resume_thread.join().expect("resume thread");

    assert!(heartbeat_outcome.drift.is_some());
    assert!(stored("d-concurrent-resume").drift.is_some());
    assert_eq!(drift_entries().len(), 1);
    assert!(audit_trail::verify_chain().valid);
}

#[test]
fn acknowledgement_rejects_stale_evidence_after_newer_drift() {
    let _iso = isolated();
    let truth = register_attested("d-stale-ack");
    seed_stored("d-stale-ack", FOREIGN, &truth.config_sha256);
    let first = heartbeat("d-stale-ack").expect("first heartbeat");
    let old_evidence = first.evidence().expect("first drift").to_string();

    seed_stored("d-stale-ack", FOREIGN_2, &truth.config_sha256);
    let second = heartbeat("d-stale-ack").expect("new heartbeat");
    let current_evidence = second.evidence().expect("second drift").to_string();
    assert_ne!(old_evidence, current_evidence);

    let error = acknowledge_drift("d-stale-ack", &old_evidence)
        .expect_err("stale acknowledgement must fail");
    assert!(error.contains("does not match"), "{error}");
    assert_eq!(
        stored("d-stale-ack").drift.expect("sticky").evidence,
        current_evidence
    );
    let ack = acknowledge_drift("d-stale-ack", &current_evidence).expect("current ack");
    assert_eq!(ack.evidence, current_evidence);
    assert_eq!(acknowledgement_entries().len(), 1);
}

#[test]
fn acknowledgement_audit_failure_leaves_drift_intact() {
    let _iso = isolated();
    let truth = register_attested("d-ack-failsafe");
    seed_stored("d-ack-failsafe", FOREIGN, &truth.config_sha256);
    let outcome = heartbeat("d-ack-failsafe").expect("heartbeat");
    let evidence = outcome.evidence().expect("drift").to_string();

    let audit_path = data_dir().join("audit").join("trail.jsonl");
    let mut permissions = std::fs::metadata(&audit_path)
        .expect("audit metadata")
        .permissions();
    let writable_permissions = permissions.clone();
    permissions.set_readonly(true);
    std::fs::set_permissions(&audit_path, permissions.clone()).expect("block audit append");
    let error = acknowledge_drift("d-ack-failsafe", &evidence)
        .expect_err("acknowledgement must fail closed");
    assert!(error.contains("audit"), "{error}");
    assert_eq!(
        stored("d-ack-failsafe")
            .drift
            .expect("drift remains")
            .evidence,
        evidence
    );
    assert!(
        check_with_policy(
            "d-ack-failsafe",
            DriftPolicy {
                block_on_drift: true
            }
        )
        .drifted
    );

    std::fs::set_permissions(&audit_path, writable_permissions).expect("unblock audit append");
    acknowledge_drift("d-ack-failsafe", &evidence).expect("retry acknowledgement");
    assert!(stored("d-ack-failsafe").drift.is_none());
    assert_eq!(acknowledgement_entries().len(), 1);
}

#[test]
fn block_on_drift_never_overrides_a_suspended_identity() {
    let _iso = isolated();
    register_attested("d-suspended");
    seed_stored("d-suspended", FOREIGN, FOREIGN);
    heartbeat("d-suspended").expect("heartbeat").expect("drift");
    suspend("d-suspended", "incident review").expect("suspend");

    let permissive = check_with_policy("d-suspended", DriftPolicy::default());
    assert!(!permissive.allowed, "suspension still denies");
    assert!(permissive.drifted, "suspension must not hide the drift");
    assert!(
        permissive.detail.contains("incident review"),
        "{permissive:?}"
    );
    // Suspending is not an acknowledgement.
    assert!(stored("d-suspended").drift.is_some());
}

#[test]
fn audit_sink_failure_aborts_the_attestation_update_and_stays_recoverable() {
    let _iso = isolated();
    register_attested("d-failsafe");
    seed_stored("d-failsafe", FOREIGN, FOREIGN);
    let before = stored("d-failsafe");
    let before_heartbeat = get("d-failsafe").expect("record").last_heartbeat;

    // Block appends while preserving the existing chain. Deleting the
    // trail would be evidence loss and must remain unrecoverable.
    let audit_path = data_dir().join("audit").join("trail.jsonl");
    let mut permissions = std::fs::metadata(&audit_path)
        .expect("audit metadata")
        .permissions();
    let writable_permissions = permissions.clone();
    permissions.set_readonly(true);
    std::fs::set_permissions(&audit_path, permissions.clone()).expect("block audit append");

    let error = heartbeat("d-failsafe").expect_err("must fail closed");
    assert!(error.contains("attestation not updated"), "{error}");

    let after = stored("d-failsafe");
    assert_eq!(
        after.binary_sha256, before.binary_sha256,
        "no silent state update without audit evidence"
    );
    assert_eq!(after.config_sha256, before.config_sha256);
    assert!(after.drift.is_none());
    assert_eq!(
        get("d-failsafe").expect("record").last_heartbeat,
        before_heartbeat,
        "liveness must not advance either"
    );

    // Recoverable and deterministic: the same drift is re-detected.
    std::fs::set_permissions(&audit_path, writable_permissions).expect("unblock audit append");
    let evidence = heartbeat("d-failsafe").expect("retry").expect("drift");
    assert!(evidence.contains("dimensions=binary+config"), "{evidence}");
    assert_eq!(drift_entries().len(), 1);
    assert!(audit_trail::verify_chain().valid);
}

#[cfg(unix)]
#[test]
fn failed_registry_persist_leaves_the_previous_snapshot_intact() {
    use std::os::unix::fs::PermissionsExt;

    let _iso = isolated();
    register_attested("d-atomic");
    let path = registry_path().expect("registry path");
    let before = std::fs::read_to_string(&path).expect("snapshot");
    let dir = path.parent().expect("agents dir").to_path_buf();

    let original = std::fs::metadata(&dir).expect("meta").permissions();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500))
        .expect("make dir read-only");
    let mode_ignored = std::fs::File::create(dir.join(".probe")).is_ok();
    let result = with_registry(|reg| {
        reg.remove("d-atomic");
        Ok(())
    });
    std::fs::set_permissions(&dir, original).expect("restore permissions");
    let _ = std::fs::remove_file(dir.join(".probe"));

    if mode_ignored {
        return; // running as root: the OS ignores the mode bits.
    }
    let error = result.expect_err("persist must fail closed");
    assert!(error.contains("persist registry"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("registry still readable"),
        before,
        "a failed write must not truncate or corrupt the registry"
    );
    assert!(
        !dir.join("identity-registry.json.tmp").exists(),
        "temp file leaked"
    );
    assert!(
        get("d-atomic").is_some(),
        "the record must survive the failed write"
    );
}

#[test]
fn older_writer_cannot_erase_unacknowledged_drift_from_audit_history() {
    let _iso = isolated();
    register_attested("d-downgrade");
    seed_stored("d-downgrade", FOREIGN, FOREIGN);
    let outcome = heartbeat("d-downgrade").expect("observe drift");
    assert!(outcome.drift.is_some());

    // Simulate a pre-S3 writer: it accepts unknown fields while reading,
    // then serializes the old shape and therefore drops `drift`.
    let path = registry_path().expect("registry path");
    let mut registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("registry"))
            .expect("registry json");
    registry["d-downgrade"]["attestation"]
        .as_object_mut()
        .expect("attestation object")
        .remove("drift");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&registry).expect("old writer serialization"),
    )
    .expect("old writer rewrite");

    let check = check_with_policy(
        "d-downgrade",
        DriftPolicy {
            block_on_drift: true,
        },
    );
    assert!(check.drifted, "audit history must restore the sticky mark");
    assert!(!check.allowed, "downgrade rewrite must remain fail-closed");
}

#[test]
fn hostile_stored_drift_evidence_is_never_returned_or_persisted() {
    let _iso = isolated();
    register_attested("d-hostile-evidence");
    let path = registry_path().expect("registry path");
    let mut registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("registry"))
            .expect("registry json");
    let hostile = "attacker\n\\\"quoted\\\"".repeat(EVIDENCE_MAX_CHARS);
    registry["d-hostile-evidence"]["attestation"]["drift"] = serde_json::json!({
        "detected_at": "2026-09-07T00:00:00Z",
        "binary": true,
        "config": false,
        "evidence": hostile,
    });
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&registry).expect("hostile serialization"),
    )
    .expect("rewrite registry");

    let check = check_with_policy("d-hostile-evidence", DriftPolicy::default());
    assert!(!check.allowed, "invalid stored evidence must fail closed");
    assert!(!check.detail.contains("attacker"), "{check:?}");
    let before = std::fs::read_to_string(&path).expect("before heartbeat");
    let error = heartbeat("d-hostile-evidence").expect_err("heartbeat must reject evidence");
    assert!(error.contains("invalid or oversized"), "{error}");
    assert_eq!(
        std::fs::read_to_string(&path).expect("after heartbeat"),
        before,
        "rejected evidence must not be persisted again"
    );
}

#[test]
fn heartbeat_rejects_a_tampered_audit_chain_even_with_stored_drift() {
    let _iso = isolated();
    let truth = register_attested("d-tampered-chain");
    seed_stored("d-tampered-chain", FOREIGN, &truth.config_sha256);
    heartbeat("d-tampered-chain").expect("observe drift");
    let before = get("d-tampered-chain").expect("record").last_heartbeat;

    let audit_path = data_dir().join("audit").join("trail.jsonl");
    let mut audit = std::fs::read_to_string(&audit_path).expect("audit trail");
    audit.push_str("{not valid json}\n");
    std::fs::write(&audit_path, audit).expect("tamper audit trail");

    let error = heartbeat("d-tampered-chain").expect_err("tampered chain must fail closed");
    assert!(error.contains("audit chain invalid"), "{error}");
    assert_eq!(
        get("d-tampered-chain").expect("record").last_heartbeat,
        before,
        "failed verification must not advance liveness"
    );
}

#[test]
fn missing_audit_trail_cannot_clear_or_mask_drift() {
    let _iso = isolated();
    let truth = register_attested("d-missing-audit");
    seed_stored("d-missing-audit", FOREIGN, &truth.config_sha256);
    let outcome = heartbeat("d-missing-audit").expect("observe drift");
    let evidence = outcome.drift.expect("sticky drift").evidence;
    let before = get("d-missing-audit").expect("record").last_heartbeat;
    std::fs::remove_file(data_dir().join("audit").join("trail.jsonl")).expect("remove audit trail");

    let heartbeat_error = heartbeat("d-missing-audit").expect_err("must fail closed");
    assert!(
        heartbeat_error.contains("audit trail unavailable"),
        "{heartbeat_error}"
    );
    assert_eq!(
        get("d-missing-audit").expect("record").last_heartbeat,
        before
    );
    let ack_error = acknowledge_drift("d-missing-audit", &evidence)
        .expect_err("missing evidence cannot be acknowledged");
    assert!(ack_error.contains("audit trail unavailable"), "{ack_error}");
    assert!(stored("d-missing-audit").drift.is_some());

    let check = check_with_policy(
        "d-missing-audit",
        DriftPolicy {
            block_on_drift: true,
        },
    );
    assert!(!check.allowed, "evidence loss must deny execution");
    assert!(
        check.drifted,
        "evidence loss must remain a red drift signal"
    );
}

#[test]
fn empty_audit_trail_cannot_clear_or_mask_drift_after_legacy_downgrade() {
    let _iso = isolated();
    let truth = register_attested("d-empty-audit");
    seed_stored("d-empty-audit", FOREIGN, &truth.config_sha256);
    let evidence = heartbeat("d-empty-audit")
        .expect("observe drift")
        .drift
        .expect("sticky drift")
        .evidence;

    let audit_path = data_dir().join("audit").join("trail.jsonl");
    std::fs::write(&audit_path, b"").expect("truncate audit trail");

    // Simulate a pre-S3 writer rewriting the current registry shape: it drops
    // the sticky field while the audit source is now an existing empty file.
    let path = registry_path().expect("registry path");
    let mut registry: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("registry"))
            .expect("registry json");
    registry["d-empty-audit"]["attestation"]
        .as_object_mut()
        .expect("attestation object")
        .remove("drift");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&registry).expect("old writer serialization"),
    )
    .expect("old writer rewrite");
    let before = std::fs::read(&path).expect("rewritten registry");

    let heartbeat_error =
        heartbeat("d-empty-audit").expect_err("empty audit must block heartbeat recovery");
    assert!(
        heartbeat_error.contains("audit trail unavailable"),
        "{heartbeat_error}"
    );

    let check = check_with_policy(
        "d-empty-audit",
        DriftPolicy {
            block_on_drift: true,
        },
    );
    assert!(!check.allowed, "empty audit must deny execution: {check:?}");
    assert!(
        check.drifted,
        "empty audit must remain a red signal: {check:?}"
    );

    let ack_error = acknowledge_drift("d-empty-audit", &evidence)
        .expect_err("registry-only evidence must not be acknowledged");
    assert!(ack_error.contains("audit trail unavailable"), "{ack_error}");
    assert_eq!(
        std::fs::read(&path).expect("registry after rejected operations"),
        before,
        "rejected heartbeat/ack must not rewrite the downgraded registry"
    );
    assert!(
        std::fs::read(&audit_path)
            .expect("audit after rejected operations")
            .is_empty(),
        "rejected acknowledgement must not append to an empty audit trail"
    );
}

#[test]
fn stripping_attestation_cannot_hide_audited_drift_after_first_heartbeat() {
    let _iso = isolated();
    let truth = register_attested("d-stripped-attestation");
    seed_stored("d-stripped-attestation", FOREIGN, &truth.config_sha256);
    let evidence = heartbeat("d-stripped-attestation")
        .expect("observe drift")
        .drift
        .expect("sticky drift")
        .evidence;
    with_registry(|registry| {
        registry
            .get_mut("d-stripped-attestation")
            .expect("record")
            .attestation = None;
        Ok(())
    })
    .expect("simulate older writer");

    let recovered = heartbeat("d-stripped-attestation")
        .expect("recover audited drift")
        .drift
        .expect("drift remains visible");
    assert_eq!(recovered.evidence, evidence);
}

#[test]
fn corrupt_registry_cannot_be_replaced_by_a_later_mutation() {
    let _iso = isolated();
    register_attested("d-corrupt");
    let path = registry_path().expect("registry path");
    let corrupt = b"{not valid json";
    std::fs::write(&path, corrupt).expect("corrupt registry");

    let error = suspend("d-corrupt", "must not overwrite").expect_err("fail closed");
    assert!(error.contains("parse registry"), "{error}");
    assert_eq!(
        std::fs::read(&path).expect("registry remains readable"),
        corrupt,
        "a corrupt snapshot must remain untouched for operator recovery"
    );
    assert!(
        !check("d-corrupt").allowed,
        "read failure must deny identity"
    );
}

/// Evidence goes verbatim into a JSONL audit line, so a hand-edited
/// registry must not be able to inject into it or blow up its size.
#[test]
fn drift_evidence_is_bounded_and_injection_proof() {
    let hostile = Attestation {
        binary_sha256: "\n\"}{ evil".repeat(512),
        config_sha256: String::new(),
        attested_at: String::new(),
        drift: None,
    };
    let fresh = Attestation {
        binary_sha256: "a".repeat(64),
        config_sha256: "b".repeat(64),
        attested_at: String::new(),
        drift: None,
    };

    let dimensions = diff_attestation(&hostile, &fresh);
    assert_eq!(
        dimensions,
        DriftDimensions {
            binary: true,
            config: true
        }
    );
    let evidence = drift_evidence(&hostile, &fresh, dimensions);
    assert!(
        evidence.len() <= 110,
        "unbounded evidence ({} chars): {evidence}",
        evidence.len()
    );
    assert!(
        !evidence.contains(['\n', '"', '\\']),
        "evidence must not be able to break the JSONL line: {evidence}"
    );
    // Only the single hex digit `e` of "\n\"}{ evil" survives verbatim.
    assert!(
        evidence.contains("binary=?????e??????->aaaaaaaaaaaa"),
        "{evidence}"
    );
    assert!(evidence.contains("config=none->bbbbbbbbbbbb"), "{evidence}");
    assert_eq!(digest_prefix(""), "none");
    assert_eq!(digest_prefix(&"c".repeat(64)), "cccccccccccc");
}
