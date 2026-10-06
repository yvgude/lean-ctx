use super::{
    AgentRecord, AgentStatus, binary_sha256, check, decommission, get, heartbeat, list,
    list_active, register, registry_path, require_active, resume, spiffe_id, suspend,
    suspend_agents_for_owner, with_registry,
};
use fs2::FileExt;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

fn isolated() -> crate::core::data_dir::IsolatedDataDir {
    // Hashing the (debug, ~400 MB) test binary is env-independent and cached per
    // process; warm it before the global env lock so no holder pays seconds.
    let _ = binary_sha256();
    crate::core::data_dir::isolated_data_dir()
}

#[test]
fn owner_is_mandatory_and_role_must_exist() {
    let _iso = isolated();
    assert!(register("a1", "coder", " ").is_err());
    assert!(register("a1", "no-such-role", "yves@org").is_err());
    assert!(register("a/1", "coder", "yves@org").is_err());
}

#[test]
fn identity_registry_does_not_overwrite_mcp_presence_registry() {
    let iso = isolated();
    let agents_dir = iso.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let presence_path = agents_dir.join("registry.json");
    let presence = r#"{"agents":[{"agent_id":"mcp-1"}]}"#;
    std::fs::write(&presence_path, presence).expect("presence registry");
    register("identity-1", "coder", "yves@org").expect("register identity");
    assert_eq!(
        std::fs::read_to_string(presence_path).expect("presence survives"),
        presence
    );
    assert!(agents_dir.join("identity-registry.json").exists());
}

#[test]
fn require_active_never_treats_presence_registry_as_identity_source() {
    let iso = isolated();
    let agents_dir = iso.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    std::fs::write(
        agents_dir.join("registry.json"),
        r#"{"agents":[],"scratchpad":[],"updated_at":"2026-01-01T00:00:00Z"}"#,
    )
    .expect("presence registry");

    let error = require_active("identity-from-presence").expect_err("presence is not identity");
    assert!(error.contains("corrupt"), "unexpected error: {error}");
}

#[test]
fn require_active_is_strict_for_missing_revoked_and_malformed_identities() {
    let iso = isolated();
    let agents_dir = iso.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");

    assert!(require_active("missing").is_err());
    register("bound", "coder", "yves@org").expect("register identity");
    assert!(require_active("bound").is_ok());
    suspend("bound", "review").expect("suspend identity");
    assert!(require_active("bound").is_err());
    resume("bound").expect("resume identity");
    decommission("bound").expect("decommission identity");
    assert!(require_active("bound").is_err());

    std::fs::write(agents_dir.join("identity-registry.json"), "{not json")
        .expect("malformed durable registry");
    let error = require_active("bound").expect_err("malformed registry fails closed");
    assert!(error.contains("corrupt"), "unexpected error: {error}");
}

#[test]
fn require_active_supports_legacy_durable_file_fallback() {
    let iso = isolated();
    let agents_dir = iso.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let legacy = AgentRecord {
        agent_id: "legacy-bound".to_string(),
        role: "coder".to_string(),
        owner: "yves@org".to_string(),
        status: AgentStatus::Active,
        created_at: String::new(),
        public_key: String::new(),
        attestation: None,
        last_heartbeat: None,
        suspended_reason: None,
        decommissioned_at: None,
    };
    let records = BTreeMap::from([(legacy.agent_id.clone(), legacy)]);
    std::fs::write(
        agents_dir.join("registry.json"),
        serde_json::to_string(&records).expect("legacy JSON"),
    )
    .expect("legacy durable registry");

    require_active("legacy-bound").expect("legacy active identity");
}

#[test]
fn legacy_identity_registry_migrates_on_next_write() {
    let iso = isolated();
    let agents_dir = iso.path().join("agents");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    let legacy_path = agents_dir.join("registry.json");
    let legacy = AgentRecord {
        agent_id: "legacy-1".to_string(),
        role: "coder".to_string(),
        owner: "yves@org".to_string(),
        status: AgentStatus::Active,
        created_at: String::new(),
        public_key: String::new(),
        attestation: None,
        last_heartbeat: None,
        suspended_reason: None,
        decommissioned_at: None,
    };
    let records = BTreeMap::from([(legacy.agent_id.clone(), legacy)]);
    let mut legacy_json = serde_json::to_value(&records).expect("JSON");
    legacy_json["legacy-1"]["pid"] = serde_json::json!(99_999_999_u32);
    legacy_json["legacy-1"]["last_seen"] = serde_json::json!("2026-01-01T00:00:00Z");
    std::fs::write(
        &legacy_path,
        serde_json::to_string(&legacy_json).expect("legacy JSON"),
    )
    .expect("legacy registry");
    assert!(get("legacy-1").is_some());
    heartbeat("legacy-1").expect("migrate heartbeat");
    assert!(agents_dir.join("identity-registry.json").exists());
    let migrated = std::fs::read_to_string(agents_dir.join("identity-registry.json"))
        .expect("migrated registry");
    assert!(!migrated.contains("\"pid\""));
    assert!(!migrated.contains("\"last_seen\""));
}

#[test]
fn lifecycle_register_suspend_resume_decommission() {
    let _iso = isolated();
    let rec = register("agent-x", "coder", "yves@org").expect("register");
    assert_eq!(rec.status, AgentStatus::Active);
    assert_eq!(rec.public_key.len(), 64);
    assert!(rec.attestation.is_some());
    assert!(register("agent-x", "coder", "yves@org").is_err());
    assert!(check("agent-x").allowed);
    suspend("agent-x", "incident review").expect("suspend");
    assert!(!check("agent-x").allowed);
    resume("agent-x").expect("resume");
    assert!(check("agent-x").allowed);
    decommission("agent-x").expect("decommission");
    assert!(!check("agent-x").allowed);
    assert!(resume("agent-x").is_err());
    assert!(get("agent-x").expect("kept").decommissioned_at.is_some());
}

#[test]
fn owner_offboarding_suspends_only_their_active_agents() {
    let _iso = isolated();
    register("a-alice-1", "coder", "alice@org").expect("r1");
    register("a-alice-2", "reviewer", "alice@org").expect("r2");
    register("a-bob-1", "coder", "bob@org").expect("r3");
    decommission("a-alice-2").expect("gone");
    let hit = suspend_agents_for_owner("alice@org", "SCIM deactivated").expect("offboard");
    assert_eq!(hit, vec!["a-alice-1".to_string()]);
    assert_eq!(get("a-bob-1").expect("bob").status, AgentStatus::Active);
    assert_eq!(
        get("a-alice-1").expect("alice").suspended_reason.as_deref(),
        Some("SCIM deactivated")
    );
}

#[test]
fn unregistered_agents_are_flagged() {
    let _iso = isolated();
    let check = check("ghost");
    assert!(!check.registered);
    assert!(!check.allowed);
}

#[test]
fn spiffe_id_shape() {
    let record = AgentRecord {
        agent_id: "ci-7".to_string(),
        role: "coder".to_string(),
        owner: "ops@org".to_string(),
        status: AgentStatus::Active,
        created_at: String::new(),
        public_key: String::new(),
        attestation: None,
        last_heartbeat: None,
        suspended_reason: None,
        decommissioned_at: None,
    };
    assert_eq!(
        spiffe_id(&record, "org.example"),
        "spiffe://org.example/agent/coder/ci-7"
    );
}

#[test]
fn heartbeat_updates_liveness_and_reports_no_false_drift() {
    let _iso = isolated();
    register("hb-1", "coder", "yves@org").expect("register");
    let drift = heartbeat("hb-1").expect("heartbeat");
    assert!(
        drift.is_none(),
        "same binary+config must not drift: {drift:?}"
    );
    assert!(get("hb-1").expect("rec").last_heartbeat.is_some());
    assert!(heartbeat("ghost").is_err());
}

#[test]
fn list_active_excludes_historical_identities() {
    let _iso = isolated();
    register("active", "coder", "yves@org").expect("active register");
    register("suspended", "coder", "yves@org").expect("suspended register");
    register("closed", "coder", "yves@org").expect("closed register");
    suspend("suspended", "test").expect("suspend");
    decommission("closed").expect("decommission");

    assert_eq!(
        list_active()
            .into_iter()
            .map(|record| record.agent_id)
            .collect::<Vec<_>>(),
        vec!["active"]
    );
    assert_eq!(list().len(), 3);
}

#[test]
fn registry_lock_contention_times_out_instead_of_blocking_indefinitely() {
    let _iso = isolated();
    let path = registry_path().expect("registry path");
    let lock_path = path.with_extension("lock");
    let held_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .expect("lock file");
    held_lock.lock_exclusive().expect("hold lock");

    let started = Instant::now();
    let error = with_registry(|_| Ok(())).expect_err("contention must fail closed");

    assert!(error.contains("timed out"), "unexpected error: {error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded lock exceeded two seconds"
    );
}
