// SPDX-License-Identifier: Apache-2.0
//! Unified Control on the engine against the C0 2.x policy fixtures.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::core::hive::effective_floor::{self, Floor};
use crate::core::hive::keyset_v3;

const VAULT: &str = "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42";
/// After the policy key's `created_at`, before the bundle's `not_after`.
const RECEIVED_AT: u64 = 1_792_800_000_000;

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path)
}

fn raw() -> Value {
    fixture("routes/policy_bundle.v2.response.json")["effective_policy"].clone()
}

fn response() -> Value {
    fixture("routes/policy_bundle.v2.response.json")
}

fn root(name: &str) -> [u8; 32] {
    let keys = fixture("vectors/signing.vectors.json");
    let mut out = [0u8; 32];
    hex::decode_to_slice(
        keys["keys"][name]["public_key_hex"].as_str().unwrap(),
        &mut out,
    )
    .unwrap();
    out
}

fn keyset() -> KeySetV3 {
    keyset_v3::verify(
        &fixture("routes/keyset.v3.response.json"),
        &[root("root_new")],
        &[root("root_old")],
    )
    .unwrap()
}

/// The 2.x lease fixture, issued to the bundle's organization and engine.
fn lease() -> HiveLeaseV1 {
    let mut lease = fixture("routes/lease_envelope.for_2x.response.json")["lease"].clone();
    lease["org_id"] = raw()["org_id"].clone();
    lease["instance_id"] = raw()["instance_id"].clone();
    serde_json::from_value(lease).unwrap()
}

fn time(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

fn accepted(dir: &Path) -> PolicyState {
    let mut state = PolicyState::default();
    receive(
        dir,
        &response(),
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert_eq!(state.effective_refusal, None);
    state
}

fn store() -> (tempfile::TempDir, crate::core::hive::HiveStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::core::hive::HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    (dir, store)
}

/// A full answer as `policy::pull` takes it after its 1.x part: receive
/// the effective policy, then settle the ETag and the policy holds.
fn pulled(store: &crate::core::hive::HiveStore, state: &mut PolicyState, body: &Value) -> bool {
    let previous = state.clone();
    let taken = receive(
        store.data_dir(),
        body,
        Some(&keyset()),
        &lease(),
        state,
        RECEIVED_AT,
    );
    crate::core::hive::policy::settle(store, &previous, state, taken);
    taken
}

#[test]
fn the_signed_bundle_verifies_once_and_then_stays_the_same() {
    let verdict = verify(&raw(), &keyset(), &lease(), None, RECEIVED_AT).unwrap();
    let Verdict::New(bundle) = verdict else {
        panic!("a first bundle is new")
    };
    assert_eq!((bundle.version, bundle.policy.vaults.len()), (3, 1));
    let dir = tempfile::tempdir().unwrap();
    let state = accepted(dir.path());
    let effective = state.effective.as_ref().unwrap();
    assert_eq!(effective.version, 3);
    assert_eq!(
        verify(&raw(), &keyset(), &lease(), Some(effective), RECEIVED_AT),
        Ok(Verdict::Same)
    );
}

#[test]
fn downgrades_foreign_bundles_wrong_keys_and_tampering_are_refused() {
    let in_force = |version, digest: &str| EffectiveState {
        version,
        digest: digest.into(),
        ..EffectiveState::default()
    };
    let digest = raw()["digest"].as_str().unwrap().to_string();
    let check = |raw: &Value, lease: &HiveLeaseV1, previous: Option<&EffectiveState>| {
        verify(raw, &keyset(), lease, previous, RECEIVED_AT).unwrap_err()
    };
    assert!(check(&raw(), &lease(), Some(&in_force(4, &digest))).contains("downgrade"));
    let other = format!("sha256:{}", "0".repeat(64));
    assert!(check(&raw(), &lease(), Some(&in_force(3, &other))).contains("downgrade"));
    let mut foreign = lease();
    foreign.instance_id = format!("inst_{}", "a".repeat(32));
    assert!(check(&raw(), &foreign, None).contains("another workspace or engine"));
    // A key of another purpose (the event key) never verifies a policy.
    let mut event_key = raw();
    event_key["key_id"] = json!("4fc7bebc302357bd");
    assert!(check(&event_key, &lease(), None).contains("missing_purpose"));
    let mut tampered = raw();
    tampered["policy"]["effective"]["context"]["max_context_tokens"] = json!(640_000);
    assert!(check(&tampered, &lease(), None).contains("signature"));
    // A policy key received before it was created is not valid yet.
    let early = verify(&raw(), &keyset(), &lease(), None, 1_000).unwrap_err();
    assert!(early.contains("revoked_key"), "{early}");
}

#[test]
fn a_refused_or_dropped_bundle_keeps_the_last_valid_policy_installed() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = accepted(dir.path());
    let before = effective_floor::read(&effective_floor::floor_dir_in(dir.path())).unwrap();
    let kept = state.effective.clone();
    // Hive drops the policy: refused, nothing changes.
    let mut dropped = response();
    dropped["effective_policy"] = Value::Null;
    receive(
        dir.path(),
        &dropped,
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert!(
        state
            .effective_refusal
            .as_deref()
            .unwrap()
            .contains("stays in force")
    );
    // A tampered one: refused, nothing changes.
    let mut tampered = response();
    tampered["effective_policy"]["policy"]["effective"]["security"]["pii"] = json!("off");
    receive(
        dir.path(),
        &tampered,
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert!(
        state
            .effective_refusal
            .as_deref()
            .unwrap()
            .contains("signature")
    );
    // No key set: refused.
    receive(
        dir.path(),
        &response(),
        None,
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert!(
        state
            .effective_refusal
            .as_deref()
            .unwrap()
            .contains("key set")
    );
    assert_eq!(state.effective, kept);
    let after = effective_floor::read(&effective_floor::floor_dir_in(dir.path())).unwrap();
    assert_eq!(before, after);
    // A 1.x answer to an engine without an effective policy: nothing at all.
    let mut fresh = PolicyState::default();
    receive(
        dir.path(),
        &dropped,
        Some(&keyset()),
        &lease(),
        &mut fresh,
        RECEIVED_AT,
    );
    assert_eq!(fresh, PolicyState::default());
}

#[test]
fn the_installed_floor_maps_every_engine_member_and_files_are_private() {
    let dir = tempfile::tempdir().unwrap();
    let _ = accepted(dir.path());
    let floor_dir = effective_floor::floor_dir_in(dir.path());
    #[cfg(unix)]
    for name in [effective_floor::FLOOR_FILE, effective_floor::REQUIRED_FILE] {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(floor_dir.join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
    let (floor, required) = effective_floor::read(&floor_dir).unwrap();
    let default = effective_floor::resolve(floor.as_deref(), required.as_deref(), None)
        .unwrap()
        .unwrap();
    assert_eq!(default.deny_tools, ["ctx_shell"]);
    assert_eq!(default.max_context_tokens, Some(64_000));
    assert_eq!(default.filters.pii.as_deref(), Some("redact"));
    assert_eq!(default.egress.block_secrets, Some(true));
    let vault = effective_floor::resolve(floor.as_deref(), required.as_deref(), Some(VAULT))
        .unwrap()
        .unwrap();
    assert_eq!(vault.filters.pii.as_deref(), Some("block"));
    // Every member the engine enforces lands on its x-engine field.
    let full: lean_ctx_protocol::hive::effective_policy::EffectivePolicyV1 =
        serde_json::from_value(json!({
            "schema": "leanctx.hive.policy/v1",
            "security": {"block_secrets": true, "pii": "block", "classification": "warn",
                "blocked_labels": ["customer"], "allowed_models": [["claude-*"]]},
            "context": {"max_context_tokens": 1000, "injection_filter": "redact",
                "read_mode": "map", "allowed_tools": ["ctx_read"], "denied_tools": ["ctx_shell"],
                "max_writes_per_min": 5, "max_cost_cents_per_person_per_day": 250,
                "max_cost_cents_per_project_per_month": 10000,
                "max_requests_per_minute_per_person": 30},
            "sharing": {}
        }))
        .unwrap();
    full.validate().unwrap();
    let r = effective_floor::to_resolved(&full, 9);
    assert_eq!(r.filters.classification.as_deref(), Some("warn"));
    assert_eq!(r.filters.injection.as_deref(), Some("redact"));
    assert!(r.filters.blocked_labels.contains(&"customer".to_string()));
    assert!(
        r.filters
            .blocked_labels
            .contains(&"CONFIDENTIAL".to_string()),
        "a Hive label list adds to the built-in labels, never replaces them"
    );
    assert_eq!(
        r.routing.model_ceiling_groups,
        [vec!["claude-*".to_string()]]
    );
    assert!(!r.routing.model_allowed("gpt-4o"));
    assert_eq!(r.default_read_mode.as_deref(), Some("map"));
    assert_eq!(
        r.allow_tools.as_deref(),
        Some(&["ctx_read".to_string()][..])
    );
    assert_eq!(r.egress.max_writes_per_min, Some(5));
    assert_eq!(r.budgets.max_cost_usd_per_person_per_day, Some(2.5));
    assert_eq!(r.budgets.max_cost_usd_per_project_per_month, Some(100.0));
    assert_eq!(r.budgets.max_requests_per_minute_per_person, Some(30));
}

#[test]
fn a_missing_damaged_or_older_policy_in_force_never_means_no_rules() {
    let dir = tempfile::tempdir().unwrap();
    let _ = accepted(dir.path());
    let floor_dir = effective_floor::floor_dir_in(dir.path());
    let (floor, required) = effective_floor::read(&floor_dir).unwrap();
    assert!(effective_floor::check(None, None).unwrap().is_none());
    assert!(effective_floor::resolve(None, required.as_deref(), None).is_err());
    assert!(effective_floor::resolve(Some("{"), required.as_deref(), None).is_err());
    assert!(effective_floor::resolve(floor.as_deref(), Some("{"), None).is_err());
    let newer = serde_json::to_string(&json!({"version": 4})).unwrap();
    assert!(effective_floor::resolve(floor.as_deref(), Some(&newer), None).is_err());
    // A floor without a requirement still applies (it only restricts).
    assert!(
        effective_floor::resolve(floor.as_deref(), None, None)
            .unwrap()
            .is_some()
    );
    // The sensitive gate refuses while the floor is gone.
    std::fs::remove_file(floor_dir.join(effective_floor::FLOOR_FILE)).unwrap();
    assert!(sensitive_gate(dir.path(), time("2026-10-26T00:00:00Z")).is_err());
}

#[test]
fn local_settings_only_tighten_and_looser_ones_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let _ = accepted(dir.path());
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".lean-ctx")).unwrap();
    std::fs::write(
        project.path().join(".lean-ctx").join("policy.toml"),
        "name = \"local\"\nversion = \"1.0.0\"\ndescription = \"looser than Hive\"\n\
         [context]\nmax_context_tokens = 200000\nallow_tools = [\"ctx_read\", \"ctx_shell\"]\n\
         [filters]\npii = \"warn\"\n[egress]\nblock_secrets = false\n",
    )
    .unwrap();
    let floor_dir = effective_floor::floor_dir_in(dir.path());
    let (floor, required) = effective_floor::read(&floor_dir).unwrap();
    let hive = effective_floor::resolve(floor.as_deref(), required.as_deref(), None)
        .unwrap()
        .unwrap();
    let local = crate::core::policy::resolve(
        &crate::core::policy::parse_file(&project.path().join(".lean-ctx/policy.toml")).unwrap(),
    )
    .unwrap();
    let merged = effective_floor::apply(&hive, Some(&local));
    assert_eq!(merged.max_context_tokens, Some(64_000));
    assert_eq!(merged.filters.pii.as_deref(), Some("redact"));
    assert_eq!(merged.egress.block_secrets, Some(true));
    let active = crate::core::policy::runtime::ActivePolicy::from_resolved(merged);
    assert!(!active.tool_allowed("ctx_shell"), "Hive denies ctx_shell");
    assert!(active.tool_allowed("ctx_read"));
    let ignored = effective_floor::local_overrides(dir.path(), project.path());
    assert_eq!(
        ignored,
        [
            "context.max_context_tokens: local 200000, Hive 64000 applies",
            "filters.pii: local warn, Hive redact applies",
            "egress.block_secrets: local false, Hive true applies",
            "context.allow_tools: local allows ctx_shell, Hive does not",
        ]
    );
    // A stricter local setting stays.
    let mut strict = local.clone();
    strict.max_context_tokens = Some(1000);
    strict.filters.pii = Some("block".into());
    let merged = effective_floor::apply(&hive, Some(&strict));
    assert_eq!(merged.max_context_tokens, Some(1000));
    assert_eq!(merged.filters.pii.as_deref(), Some("block"));
    let lines = status_lines(
        dir.path(),
        &PolicyState::default(),
        time("2026-10-26T00:00:00Z"),
        Some(project.path()),
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Ignored:      context.max_context_tokens"))
    );
}

#[test]
fn agents_cannot_write_the_policy_files_and_no_tool_reaches_the_writers() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = effective_floor::floor_dir().unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(effective_floor::FLOOR_FILE), "{}").unwrap();
    for name in [effective_floor::FLOOR_FILE, "new.json", "../state.json"] {
        let error = crate::core::pathjail::enforce_writable(&dir.join(name)).unwrap_err();
        assert!(
            error.contains("agents cannot change them"),
            "{name}: {error}"
        );
    }
    // The writers are called from the policy pull only: no MCP tool or
    // server handler names them.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let writers = [
        "effective_policy::install",
        "effective_policy::receive",
        "effective_policy::on_pull",
        "hive::policy::pull",
        "write_private(",
    ];
    let mut stack = vec![src.join("tools"), src.join("server")];
    let mut scanned = 0;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for writer in writers {
                assert!(!text.contains(writer), "{} calls {writer}", path.display());
            }
            scanned += 1;
        }
    }
    assert!(scanned > 50, "the tool sources were scanned");
}

#[test]
fn sensitive_operations_fail_closed_once_the_authorisation_expired() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        sensitive_gate(dir.path(), time("2030-01-01T00:00:00Z")).is_ok(),
        "no policy"
    );
    let _ = accepted(dir.path());
    assert!(sensitive_gate(dir.path(), time("2026-11-08T11:59:59Z")).is_ok());
    let expired = sensitive_gate(dir.path(), time("2026-11-08T12:00:00Z")).unwrap_err();
    assert!(
        expired.contains("expired at 2026-11-08T12:00:00Z"),
        "{expired}"
    );
    // Through the consent broker: a sensitive kind is refused before any
    // record is looked at; another kind is not gated.
    let store = crate::core::hive::HiveStore::at(dir.path().join("config"), dir.path());
    let broker = crate::core::hive::consent::Broker::open(&store, Default::default());
    let after = 1_794_200_000_000; // 2026-11-09T04:53:20Z
    let refused = broker
        .require(
            crate::core::hive::consent::Kind::LinkVault,
            &json!({}),
            after,
        )
        .unwrap_err();
    assert_eq!(refused.code(), "policy_authorisation_expired");
    let other = broker
        .require(
            crate::core::hive::consent::Kind::ApiKeySet,
            &json!({}),
            after,
        )
        .unwrap_err();
    assert_ne!(other.code(), "policy_authorisation_expired");
    let before = 1_792_800_000_000;
    let fine = broker
        .require(
            crate::core::hive::consent::Kind::LinkVault,
            &json!({}),
            before,
        )
        .unwrap_err();
    assert_ne!(fine.code(), "policy_authorisation_expired");
}

/// A scripted Hive for the ack: records every request.
fn fake(
    answer: impl Fn(&Value) -> (u16, Value) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let answer = Arc::new(answer);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            assert!(line.starts_with("POST /v1/policy/ack "), "{line}");
            let (status, reply) = answer(&body);
            recorded.lock().unwrap().push(body);
            let text = reply.to_string();
            let _ = write!(
                writer,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
        }
    });
    (origin, seen)
}

#[test]
fn the_applied_version_is_acked_once_with_its_unenforced_members() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = accepted(dir.path()).effective.unwrap();
    let (origin, seen) = fake(|_| (200, fixture("routes/policy_ack.response.json")));
    let client = crate::core::hive::transport::HiveClient::new(&origin).unwrap();
    ack_with(&client, &mut state);
    assert_eq!(state.ack_error, None);
    assert_eq!(state.ack.as_ref().unwrap().state, "acked");
    ack_with(&client, &mut state);
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "an acked version is not acked again");
    // The C0 request is an engine's example ack: its members are either
    // `x-enforcement: server` (POLICY-B3, Hive enforces them) or ones this
    // engine enforces (`context.max_context_tokens`), so this ack names none.
    let mut want = fixture("routes/policy_ack.request.json");
    let mut members: Vec<String> = serde_json::from_value(want["unenforced"].clone()).unwrap();
    members.retain(|m| {
        !lean_ctx_protocol::hive::effective_policy::is_server_member(m)
            && !lean_ctx_protocol::hive::effective_policy::is_engine_member(m)
    });
    members.sort();
    if members.is_empty() {
        want.as_object_mut().unwrap().remove("unenforced");
    } else {
        want["unenforced"] = json!(members);
    }
    assert_eq!(requests[0], want);
    assert_eq!(
        state.by_hive,
        [
            "security.excluded_paths",
            "sharing.auto_merge_results",
            "sharing.consent_required_for",
            "sharing.cross_team_sharing"
        ]
    );
}

#[test]
fn a_failed_ack_is_recorded_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = accepted(dir.path()).effective.unwrap();
    let (origin, seen) = fake(|_| (409, fixture("routes/policy_ack.error.policy_mismatch.json")));
    let client = crate::core::hive::transport::HiveClient::new(&origin).unwrap();
    ack_with(&client, &mut state);
    assert!(state.ack.is_none());
    assert!(
        state
            .ack_error
            .as_deref()
            .unwrap()
            .starts_with("policy_mismatch")
    );
    ack_with(&client, &mut state);
    assert_eq!(seen.lock().unwrap().len(), 2, "retried while not acked");
    // An answer for another version is no ack either.
    let (origin, _) = fake(|_| {
        let mut other = fixture("routes/policy_ack.response.json");
        other["version"] = json!(2);
        (200, other)
    });
    let client = crate::core::hive::transport::HiveClient::new(&origin).unwrap();
    ack_with(&client, &mut state);
    assert!(state.ack.is_none());
    assert!(state.ack_error.is_some());
}

#[test]
fn status_lines_are_deterministic_and_show_ack_unenforced_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = accepted(dir.path());
    let at = time("2026-10-26T00:00:00Z");
    let lines = status_lines(dir.path(), &state, at, None);
    assert_eq!(lines, status_lines(dir.path(), &state, at, None));
    assert_eq!(
        lines,
        [
            "Policy:       v3 sha256:a07526bf5e03 (valid), applied until 2026-11-08T12:00:00Z",
            "Policy ack:   pending",
            "By Hive:     security.excluded_paths, sharing.auto_merge_results, sharing.consent_required_for, sharing.cross_team_sharing (Hive refuses, this engine does not check)",
            "Unenforced:   none",
        ]
    );
    let effective = state.effective.as_mut().unwrap();
    effective.ack = Some(AckRecord {
        version: 3,
        digest: effective.digest.clone(),
        state: "acked".into(),
        acked_at: "2026-10-25T12:00:05Z".into(),
    });
    let expired = status_lines(dir.path(), &state, time("2026-11-09T00:00:00Z"), None);
    assert_eq!(
        expired[0],
        "Policy:       v3 sha256:a07526bf5e03 (EXPIRED), its rules still apply; sensitive operations refused since 2026-11-08T12:00:00Z"
    );
    assert_eq!(expired[1], "Policy ack:   acked at 2026-10-25T12:00:05Z");
    let floor: Floor = serde_json::from_str(
        &effective_floor::read(&effective_floor::floor_dir_in(dir.path()))
            .unwrap()
            .0
            .unwrap(),
    )
    .unwrap();
    assert_eq!(floor.version, 3);
    std::fs::remove_file(
        effective_floor::floor_dir_in(dir.path()).join(effective_floor::FLOOR_FILE),
    )
    .unwrap();
    let broken = status_lines(dir.path(), &state, at, None);
    assert!(broken[0].contains("missing"), "{}", broken[0]);
}

/// SERVER-B4: a refused bundle is reported once per (version, digest),
/// with the reason code and the bundle kept; a failed report is retried.
#[test]
fn a_refused_bundle_is_reported_once_with_its_reason_and_the_kept_bundle() {
    use lean_ctx_protocol::hive::effective_policy::PolicyRefusalReasonV1;
    let dir = tempfile::tempdir().unwrap();
    let mut state = PolicyState::default();
    let mut tampered = response();
    tampered["effective_policy"]["policy"]["effective"]["security"]["pii"] = json!("off");
    receive(
        dir.path(),
        &tampered,
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    let digest = raw()["digest"].as_str().unwrap().to_string();
    let mut record = state.effective_refused.clone().expect("recorded");
    assert_eq!(
        (record.version, record.digest.as_str()),
        (3, digest.as_str())
    );
    let (origin, _) = fake(|_| (503, json!({"error": {"code": "not_ready", "message": "x"}})));
    let client = crate::core::hive::transport::HiveClient::new(&origin).unwrap();
    report_refusal_with(&client, &mut record);
    assert!(
        record.reported_at.is_none() && record.report_error.is_some(),
        "retried later"
    );
    let (origin, seen) = fake(|request| {
        let refused = &request["refused"];
        (
            200,
            json!({"schema": "leanctx.hive.policy-ack/v1", "version": request["version"],
                "digest": request["digest"], "state": "refused",
                "reason_code": refused["reason_code"], "applied_version": refused["applied_version"],
                "applied_digest": refused["applied_digest"], "refused_at": "2026-10-25T12:00:05Z"}),
        )
    });
    let client = crate::core::hive::transport::HiveClient::new(&origin).unwrap();
    report_refusal_with(&client, &mut record);
    assert_eq!(record.reported_at.as_deref(), Some("2026-10-25T12:00:05Z"));
    assert_eq!(record.report_error, None);
    report_refusal_with(&client, &mut record);
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "reported once");
    assert_eq!(
        requests[0],
        json!({"version": 3, "digest": digest, "refused": {
            "reason_code": "signature_invalid",
            "detail": "the effective policy signature does not verify",
            "applied_version": null, "applied_digest": null}})
    );
    // The same bundle again keeps its report; a valid one ends it.
    state.effective_refused = Some(record);
    receive(
        dir.path(),
        &tampered,
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert!(
        state
            .effective_refused
            .as_ref()
            .unwrap()
            .reported_at
            .is_some()
    );
    receive(
        dir.path(),
        &response(),
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert_eq!(state.effective_refused, None);
    assert_eq!(state.effective_refusal, None);
    // A refusal names the bundle kept in force.
    let mut other = response();
    other["effective_policy"]["version"] = json!(4);
    receive(
        dir.path(),
        &other,
        Some(&keyset()),
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    let refused = &state.effective_refused.as_ref().unwrap().refused;
    assert_eq!(refused.reason_code, PolicyRefusalReasonV1::SignatureInvalid);
    assert_eq!(refused.applied_version, Some(3));
    assert_eq!(refused.applied_digest.as_deref(), Some(digest.as_str()));
    // The bundle in force itself is never reported (only its key set is
    // missing now).
    receive(
        dir.path(),
        &response(),
        None,
        &lease(),
        &mut state,
        RECEIVED_AT,
    );
    assert_eq!(state.effective_refused, None);
    assert!(
        state
            .effective_refusal
            .as_deref()
            .unwrap()
            .contains("key set")
    );
}

/// Live Release B e2e: a proxy flipped one hex digit of the signature and
/// passed Hive' ETag through. The refused copy must keep no ETag, else
/// every later pull is a 304 and the genuine bundle never arrives.
#[test]
fn a_refused_bundle_keeps_no_etag_so_the_next_pull_fetches_the_genuine_one() {
    use crate::core::hive::policy::if_none_match;
    let (_dir, store) = store();
    let pull = |state: &mut PolicyState, body: &Value, etag: &str| {
        state.etag = Some(etag.to_string());
        pulled(&store, state, body)
    };
    let mut state = PolicyState {
        etag: Some("\"before\"".into()),
        ..PolicyState::default()
    };
    assert_eq!(if_none_match(&state), Some("\"before\""));
    let mut tampered = response();
    let signature = tampered["effective_policy"]["signature"]
        .as_str()
        .unwrap()
        .to_string();
    let flipped = if signature.starts_with('0') { "1" } else { "0" };
    tampered["effective_policy"]["signature"] = json!(format!("{flipped}{}", &signature[1..]));
    assert!(!pull(&mut state, &tampered, "\"genuine\""));
    assert!(state.effective_refusal.is_some(), "refused and reported");
    assert_eq!(state.effective, None, "nothing installed");
    assert_eq!(state.etag, None, "the refused copy's ETag is not kept");
    assert_eq!(if_none_match(&state), None, "the next pull asks in full");
    // The next pull gets the genuine bundle in full and installs it.
    assert!(pull(&mut state, &response(), "\"genuine\""));
    assert_eq!(state.effective.as_ref().map(|e| e.version), Some(3));
    assert_eq!(state.effective_refusal, None);
    assert_eq!(if_none_match(&state), Some("\"genuine\""));
    // The same bundle again keeps its tag (later pulls are 304s).
    assert!(pull(&mut state, &response(), "\"genuine\""));
    assert_eq!(if_none_match(&state), Some("\"genuine\""));
    // Dropped to null while v3 is in force: not taken, fetched in full again.
    let mut dropped = response();
    dropped["effective_policy"] = Value::Null;
    assert!(!pull(&mut state, &dropped, "\"dropped\""));
    assert_eq!(if_none_match(&state), None);
    assert_eq!(state.effective.as_ref().map(|e| e.version), Some(3));
}

/// Live Release B e2e: an admin loosened `sharing.task_claim_min_role`,
/// the engine installed and acked the new bundle, and the next claim still
/// waited on the old policy's hold. A newer policy in force ends every
/// policy hold; quota throttles stay, and an unchanged policy keeps them.
#[test]
fn a_newer_policy_in_force_ends_every_policy_hold_but_no_quota() {
    use crate::core::hive::throttle::{self, Scope};
    let (_dir, store) = store();
    let now = Utc::now();
    let denied = HiveError::from_response(
        403,
        &json!({"error": {"code": "policy_denied", "message": "x",
            "member": "sharing.task_claim_min_role", "level": "workspace"}}),
    );
    let quota = HiveError::from_response(429, &fixture("routes/quota_exceeded.error.json"));
    let _ = throttle::note(&store, Scope::Tasks, denied.clone(), now);
    let _ = throttle::note(&store, Scope::Bus, denied.clone(), now);
    let _ = throttle::note(&store, Scope::Sync, quota, now);
    let held = |scope| throttle::holding(&store, scope, now).is_some();
    // A refused bundle installs nothing: the holds stay.
    let mut state = PolicyState::default();
    let mut tampered = response();
    tampered["effective_policy"]["policy"]["effective"]["security"]["pii"] = json!("off");
    assert!(!pulled(&store, &mut state, &tampered));
    assert!(held(Scope::Tasks) && held(Scope::Bus));
    // The loosened policy is installed: the next claim asks Hive again.
    assert!(pulled(&store, &mut state, &response()));
    assert_eq!(state.effective.as_ref().map(|e| e.version), Some(3));
    assert!(
        !held(Scope::Tasks),
        "the claim hold ended with the new policy"
    );
    assert!(!held(Scope::Bus), "every policy hold, on every path");
    assert!(held(Scope::Sync), "a quota throttle is no policy's answer");
    // The same version again is no change: a new refusal under it holds.
    let _ = throttle::note(&store, Scope::Tasks, denied, now);
    assert!(pulled(&store, &mut state, &response()));
    assert!(held(Scope::Tasks));
}

/// A knowledge draft Hive refused by policy waits in the outbox; like the
/// claim and publish holds, a newer policy in force ends that wait at
/// once. A draft waiting on a quota keeps its wait, and an unchanged
/// policy releases nothing.
#[test]
fn a_newer_policy_in_force_releases_a_policy_held_draft_but_no_quota_wait() {
    use crate::core::hive::outbox::{CHANNEL_KNOWLEDGE, Outbox, OutboxItem};
    use crate::core::hive::throttle::POLICY_HOLD_S;
    let (_dir, store) = store();
    let now = Utc::now();
    let outbox = Outbox::at(store.data_dir());
    let hold = Some(POLICY_HOLD_S.unsigned_abs());
    let item = |id: &str| -> OutboxItem {
        outbox
            .list(CHANNEL_KNOWLEDGE)
            .into_iter()
            .find(|i| i.id == id)
            .unwrap()
    };
    let policy_id = outbox
        .enqueue(CHANNEL_KNOWLEDGE, json!({ "label": "policy" }), now)
        .unwrap();
    let quota_id = outbox
        .enqueue(CHANNEL_KNOWLEDGE, json!({ "label": "quota" }), now)
        .unwrap();
    outbox
        .hold_for_policy(&mut item(&policy_id), "policy_denied", hold, now)
        .unwrap();
    outbox
        .retry_later(&mut item(&quota_id), "quota_exceeded", Some(86_400), now)
        .unwrap();
    let due = |id: &str| item(id).due(now);
    assert!(!due(&policy_id) && !due(&quota_id), "both wait");
    // A refused bundle installs nothing: both keep waiting.
    let mut state = PolicyState::default();
    let mut tampered = response();
    tampered["effective_policy"]["policy"]["effective"]["security"]["pii"] = json!("off");
    assert!(!pulled(&store, &mut state, &tampered));
    assert!(!due(&policy_id), "no new policy, the hold stays");
    // The newer policy is installed: the policy-held draft goes again.
    assert!(pulled(&store, &mut state, &response()));
    assert!(
        due(&policy_id),
        "a newer policy ends the draft's policy wait"
    );
    assert!(!item(&policy_id).policy_hold);
    assert!(!due(&quota_id), "a quota wait is no policy's answer");
    // A new refusal under the same version holds until its time.
    outbox
        .hold_for_policy(&mut item(&policy_id), "policy_denied", hold, now)
        .unwrap();
    assert!(pulled(&store, &mut state, &response()));
    assert!(!due(&policy_id), "the same policy again releases nothing");
}
