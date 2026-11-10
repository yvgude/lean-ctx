// SPDX-License-Identifier: Apache-2.0
//! Background sync and policy enforcement evidence (3.12.3), as Local Read
//! kinds. Read only, from local files; nothing contacts Hive.
//!
//! - `hive.sync`: `lean-ctx hive sync status --json`, the daemon's
//!   background vault sync (`<data>/hive/sync/status.json`).
//! - `policy.sessions`: `lean-ctx hive policy sessions --json`, the policy
//!   installed here and which version each app session ran under.
//! - `policy.session`: `lean-ctx hive policy session --session ID --json`,
//!   one session's evidence with its receipt ids.
//!
//! Wired as one `local_read::Part`: its routes, kinds and commands are
//! chained into the route table and the capabilities builder.

use serde::Serialize;

use lean_ctx_hive::HiveStore;
use lean_ctx_hive::capture::sharing::RejectedFact;
use lean_ctx_hive::policy_sessions::{self, AppliedRow, Installed};
use lean_ctx_hive::sync::auto::{SyncStatus, VaultSyncStatus};

use super::routes::{Method, Project, Route};
use super::{Answer, Code, Failure, Via};

/// This part's routes (Route as in `routes.rs`).
pub(crate) const ROUTES: &[Route] = &[
    Route {
        kind: "hive.sync",
        method: Method::Get,
        path: "/v1/local/hive/sync",
        cli: &["hive", "sync", "status", "--json"],
        action: &["status"],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "policy.sessions",
        method: Method::Get,
        path: "/v1/local/hive/policy/sessions",
        cli: &["hive", "policy", "sessions", "--json"],
        action: &["sessions"],
        params: &[],
        project: Project::None,
        names: None,
    },
    Route {
        kind: "policy.session",
        method: Method::Get,
        path: "/v1/local/hive/policy/sessions/{id}",
        cli: &["hive", "policy", "session", "<id>", "--json"],
        action: &["session"],
        params: &[],
        project: Project::None,
        names: None,
    },
];

/// This part's document kinds.
pub(crate) const KINDS: &[&str] = &["hive.sync", "policy.session", "policy.sessions"];

/// Command path → CLI verbs, as `capabilities::COMMANDS`.
pub(crate) const COMMANDS: &[(&str, &[&str])] = &[
    ("hive.policy", &["session", "sessions"]),
    ("hive.sync", &["status"]),
];

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SyncDoc {
    pub running: bool,
    pub interval_s: u64,
    pub last_pass_at: Option<String>,
    pub vaults: Vec<VaultRow>,
}

/// One vault's sync state, with the facts the capture gate kept on this
/// machine (fix them and remember again; the next sync shares them).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct VaultRow {
    #[serde(flatten)]
    pub status: VaultSyncStatus,
    pub rejected: Vec<RejectedFact>,
}

/// The `hive.sync` body of `status` at `now`; `rejected` gives a vault's
/// rejected facts still waiting for a fix.
pub(crate) fn sync_doc(
    status: SyncStatus,
    now: chrono::DateTime<chrono::Utc>,
    rejected: &dyn Fn(&str) -> Vec<RejectedFact>,
) -> SyncDoc {
    SyncDoc {
        running: status.running_at(now),
        interval_s: status.interval_s,
        last_pass_at: status.last_pass_at,
        vaults: status
            .vaults
            .into_iter()
            .map(|status| VaultRow {
                rejected: rejected(&status.vault_id),
                status,
            })
            .collect(),
    }
}

/// The rejected facts of the vault `vault_id` (a place id) on this machine.
fn rejected_of(hive_data: &std::path::Path, vault_id: &str) -> Vec<RejectedFact> {
    use lean_ctx_hive::sync::place::place_id;
    let map = lean_ctx_hive::vault::VaultMap::load(hive_data);
    let knowledge = map
        .vaults
        .values()
        .find(|record| place_id(&record.vault_id) == vault_id)
        .and_then(lean_ctx_hive::sync::auto::root_of)
        .and_then(|root| lean_ctx::core::knowledge::ProjectKnowledge::load(&root));
    lean_ctx_hive::capture::sharing::rejected_now(hive_data, vault_id, knowledge.as_ref())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct InstalledDoc {
    pub version: Option<u64>,
    pub digest: Option<String>,
    pub key_id: Option<String>,
    pub signature: String,
    pub state: String,
    pub acked: bool,
    pub acked_at: Option<String>,
    /// `nudge`, `poll` or `manual`: what started the last policy pull.
    pub pulled_via: Option<String>,
    pub last_pull_at: Option<String>,
    /// Hive sends policy nudges: a change applies at once; otherwise the
    /// periodic pull brings it (within `pull_interval_minutes`).
    pub nudges: bool,
    /// One row per vault the policy reaches on this machine (sorted by
    /// `vault_id`): every vault linked here, and every vault with a
    /// vault-level entry in the installed policy.
    pub vaults: Vec<PolicyVault>,
    /// Whether this policy is enforced here, and by which sessions.
    pub enforcement: Enforcement,
}

/// The installed policy as enforced on this machine (D4).
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct Enforcement {
    /// The version enforced: verified, in force and acknowledged by Hive
    /// for this version and digest; `null` otherwise (an expired,
    /// unverified or unacknowledged policy is not reported as enforced).
    pub applied_version: Option<u64>,
    /// When Hive acknowledged that version (`null` with it).
    pub acknowledged_at: Option<String>,
    /// The live app sessions (a receipt within the last 15 minutes) whose
    /// newest receipt was made under the installed verified policy in
    /// force; sorted.
    pub sessions: Vec<String>,
    /// When this Instance reported `applied_version` to Hive on the bus
    /// (`policy.applied`); `null` until then.
    pub reported_at: Option<String>,
}

fn enforcement(
    hive_data: &std::path::Path,
    installed: &Installed,
    now: chrono::DateTime<chrono::Utc>,
) -> Enforcement {
    let enforced =
        installed.signature == "verified" && installed.state == "in_force" && installed.acked;
    let applied_version = installed.version.filter(|_| enforced);
    let reported_at = policy_sessions::load_reported(hive_data)
        .filter(|r| {
            Some(r.version) == applied_version && installed.digest.as_ref() == Some(&r.digest)
        })
        .map(|r| r.reported_at);
    Enforcement {
        applied_version,
        acknowledged_at: installed.acked_at.clone().filter(|_| enforced),
        sessions: policy_sessions::enforcing(hive_data, installed, now),
        reported_at,
    }
}

/// One vault under the installed policy, so a reader matches a project
/// (one of `roots`) to the policy that governs it.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct PolicyVault {
    pub vault_id: String,
    /// The vault's workspace namespace and name; `null` for a vault only
    /// the policy names (no checkout here is linked to it).
    pub ns: Option<String>,
    pub name: Option<String>,
    /// The local checkouts linked to it (sorted).
    pub roots: Vec<String>,
    /// `vault`: the policy's own entry for this vault applies; `default`:
    /// the workspace default applies; `unverified`: the installed policy
    /// files cannot be checked (fails closed, see `signature`).
    pub policy: &'static str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SessionRow {
    pub session_id: String,
    pub version: Option<u64>,
    pub digest: Option<String>,
    pub signature: String,
    pub state: String,
    pub receipts: u64,
    pub last_at: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SessionsDoc {
    /// `null`: no Hive policy installed on this machine.
    pub installed: Option<InstalledDoc>,
    pub sessions: Vec<SessionRow>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SessionDoc {
    pub session_id: String,
    pub applied: Vec<AppliedRow>,
}

/// The vaults of `map` and of the installed policy's vault-level entries
/// (`vault_entries`: `None` when the installed files cannot be checked).
fn policy_vaults(
    map: &lean_ctx::core::vault_map::VaultMap,
    vault_entries: Option<&[String]>,
) -> Vec<PolicyVault> {
    let policy_of = |vault_id: &str| match vault_entries {
        Some(ids) if ids.iter().any(|id| id == vault_id) => "vault",
        Some(_) => "default",
        None => "unverified",
    };
    let mut rows: std::collections::BTreeMap<String, PolicyVault> =
        std::collections::BTreeMap::new();
    for record in map.vaults.values() {
        let row = rows
            .entry(record.vault_id.clone())
            .or_insert_with(|| PolicyVault {
                vault_id: record.vault_id.clone(),
                ns: Some(record.ns.clone()),
                name: Some(record.name.clone()),
                roots: Vec::new(),
                policy: policy_of(&record.vault_id),
            });
        for root in &record.roots {
            if !row.roots.contains(root) {
                row.roots.push(root.clone());
            }
        }
        row.roots.sort();
    }
    for vault_id in vault_entries.unwrap_or_default() {
        rows.entry(vault_id.clone()).or_insert_with(|| PolicyVault {
            vault_id: vault_id.clone(),
            ns: None,
            name: None,
            roots: Vec::new(),
            policy: "vault",
        });
    }
    rows.into_values().collect()
}

/// The vault ids with a vault-level entry in the policy installed in
/// `hive_data` (empty: none installed); `None` when the installed files
/// cannot be checked.
fn vault_entries(hive_data: &std::path::Path) -> Option<Vec<String>> {
    use lean_ctx_hive::effective_floor;
    let (floor, required) =
        effective_floor::read(&effective_floor::floor_dir_in(hive_data)).ok()?;
    let floor = effective_floor::check(floor.as_deref(), required.as_deref()).ok()?;
    Some(floor.map_or_else(Vec::new, |floor| {
        floor
            .policy
            .vaults
            .into_iter()
            .map(|v| v.vault_id)
            .collect()
    }))
}

fn installed_doc(
    installed: Installed,
    pulls: &lean_ctx_hive::policy::PullRecord,
    vaults: Vec<PolicyVault>,
    enforcement: Enforcement,
) -> Option<InstalledDoc> {
    if installed.signature == "none" {
        return None;
    }
    let via = pulls.pulled_via.and_then(|via| {
        serde_json::to_value(via)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
    });
    Some(InstalledDoc {
        version: installed.version,
        digest: installed.digest,
        key_id: installed.key_id,
        signature: installed.signature,
        state: installed.state,
        acked: installed.acked,
        acked_at: installed.acked_at,
        pulled_via: via,
        last_pull_at: pulls.last_pull_at.clone(),
        nudges: pulls.nudges_seen,
        vaults,
        enforcement,
    })
}

/// The `policy.sessions` body of the Hive data in `hive_data` at `now`.
pub(crate) fn sessions_doc(
    hive_data: &std::path::Path,
    now: chrono::DateTime<chrono::Utc>,
) -> SessionsDoc {
    let policy = policy_sessions::installed(hive_data, now);
    let enforced = enforcement(hive_data, &policy, now);
    let installed = installed_doc(
        policy,
        &lean_ctx_hive::policy::load_pulls(hive_data),
        policy_vaults(
            &lean_ctx::core::vault_map::VaultMap::load(hive_data),
            vault_entries(hive_data).as_deref(),
        ),
        enforced,
    );
    let sessions = policy_sessions::list(hive_data)
        .into_iter()
        .filter_map(|evidence| {
            let row = evidence.applied.last()?;
            Some(SessionRow {
                session_id: evidence.session_id.clone(),
                version: row.version,
                digest: row.digest.clone(),
                signature: row.signature.clone(),
                state: row.state.clone(),
                receipts: evidence.applied.iter().map(|r| r.receipts).sum(),
                last_at: row.last_at.clone(),
            })
        })
        .collect();
    SessionsDoc {
        installed,
        sessions,
    }
}

fn store() -> Result<HiveStore, Failure> {
    HiveStore::open().map_err(Failure::from)
}

/// The session a `policy session` request names: `--session ID`, or the
/// positional after `session` (the route's `{id}`).
fn named_session(args: &[String]) -> Result<String, Failure> {
    let raw = match super::flag(args, "--session")? {
        Some(id) => id.to_owned(),
        None => super::positionals(args)
            .into_iter()
            .skip_while(|word| *word != "session")
            .nth(1)
            .map(str::to_owned)
            .ok_or_else(|| Failure::invalid("policy session needs --session ID"))?,
    };
    policy_sessions::session_id(&raw)
        .ok_or_else(|| Failure::invalid("a session id is 1-128 characters of A-Z a-z 0-9 . _ : -"))
}

/// The release root of these machine-wide documents: no single project.
const NO_PROJECT: &str = "";

/// The document of `kind` for `args` (same bytes on the CLI and the
/// socket), released through the tool run of its tool (one receipt for the
/// bytes sent).
pub(crate) fn answer(kind: &str, args: &[String], via: Via) -> Answer {
    let now = chrono::Utc::now();
    match kind {
        "hive.sync" => super::answer_released("ctx_agent", NO_PROJECT, kind, via, || {
            let store = store()?;
            Ok(sync_doc(
                lean_ctx_hive::sync::auto::load_status(store.data_dir()),
                now,
                &|vault| rejected_of(store.data_dir(), vault),
            ))
        }),
        "policy.sessions" => super::answer_released("ctx_policy", NO_PROJECT, kind, via, || {
            let store = store()?;
            Ok(sessions_doc(store.data_dir(), now))
        }),
        "policy.session" => super::answer_released("ctx_policy", NO_PROJECT, kind, via, || {
            let id = named_session(args)?;
            let store = store()?;
            let evidence = policy_sessions::load(store.data_dir(), &id).ok_or_else(|| {
                Failure::new(Code::NotFound, format!("no evidence for session {id}"))
            })?;
            Ok(SessionDoc {
                session_id: evidence.session_id,
                applied: evidence.applied,
            })
        }),
        other => Answer::failure(&Failure::invalid(format!("unknown kind {other}"))),
    }
}

/// `lean-ctx hive sync status --json`.
pub(crate) fn run_sync_status(args: &[String]) {
    super::print(&answer("hive.sync", args, Via::Cli));
}

/// `lean-ctx hive policy sessions|session … --json`.
pub(crate) fn run_policy(args: &[String]) {
    let kind = match super::positionals(args).first().copied() {
        Some("sessions") => "policy.sessions",
        Some("session") => "policy.session",
        _ => super::fail(&Failure::invalid(
            "usage: lean-ctx hive policy sessions --json | session --session ID --json",
        )),
    };
    super::print(&answer(kind, args, Via::Cli));
}

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx::core::vault_map::{VaultMap, VaultRecord};

    #[test]
    fn a_stale_status_is_not_running_and_vaults_pass_through() {
        let now = chrono::Utc::now();
        let status = SyncStatus {
            interval_s: 300,
            heartbeat_at: Some(
                (now - chrono::Duration::seconds(600))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ),
            vaults: vec![VaultSyncStatus {
                vault_id: "v".into(),
                pending: 4,
                ..VaultSyncStatus::default()
            }],
            ..SyncStatus::default()
        };
        let doc = sync_doc(status, now, &|_| Vec::new());
        assert!(!doc.running);
        assert_eq!(doc.vaults[0].status.pending, 4);
        let text = super::super::document("hive.sync", &doc);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["contract"], "local-read/1");
        assert_eq!(value["kind"], "hive.sync");
        assert!(value["last_pass_at"].is_null(), "nulls are printed: {text}");
        assert_eq!(value["vaults"][0]["pending"], 4, "the row is flat: {text}");
        assert_eq!(value["vaults"][0]["rejected"], serde_json::json!([]));
    }

    #[test]
    fn no_policy_installed_is_null_and_sessions_list_empty() {
        let dir = tempfile::tempdir().unwrap();
        let doc = sessions_doc(dir.path(), chrono::Utc::now());
        assert_eq!(
            doc,
            SessionsDoc {
                installed: None,
                sessions: Vec::new()
            }
        );
    }

    #[test]
    fn a_session_is_named_by_flag_or_segment_and_checked() {
        let args = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            named_session(&args(&["session", "--session", "app-1", "--json"])).unwrap(),
            "app-1"
        );
        assert_eq!(
            named_session(&args(&["session", "app-2", "--json"])).unwrap(),
            "app-2"
        );
        assert!(named_session(&args(&["session", "--json"])).is_err());
        assert!(named_session(&args(&["session", "../x", "--json"])).is_err());
    }

    /// pp7: the policy evidence is on the local service and in the one
    /// capabilities document (route, kind, command verb on both transports).
    #[test]
    fn the_policy_and_sync_routes_are_served_and_listed() {
        use super::super::{capabilities, routes};
        let table = routes::table_json();
        for (kind, path, command, verb) in [
            (
                "policy.sessions",
                "/v1/local/hive/policy/sessions",
                "hive.policy",
                "sessions",
            ),
            (
                "policy.session",
                "/v1/local/hive/policy/sessions/{id}",
                "hive.policy",
                "session",
            ),
            ("hive.sync", "/v1/local/hive/sync", "hive.sync", "status"),
        ] {
            let route = routes::all()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("{path} is routed"));
            assert_eq!(route.kind, kind);
            let row = table
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["path"] == path)
                .unwrap_or_else(|| panic!("{path} is in capabilities.routes"));
            assert_eq!(row["kind"], kind);
            assert_eq!(row["method"], "GET");
            assert_eq!(row["command"], command, "{row}");
            assert_eq!(row["verb"], verb, "{row}");
            let commands = capabilities::commands_of(&table);
            assert!(
                commands[command].as_array().is_some_and(|verbs| verbs
                    .contains(&serde_json::json!({"verb": verb, "via": ["cli", "socket"]}))),
                "{command} {verb}: {commands:?}"
            );
            assert!(
                capabilities::build_with(table.clone(), routes::events_json())
                    .kinds
                    .contains(&kind),
                "{kind} in capabilities.kinds"
            );
        }
        // The `{id}` segment becomes the session the CLI reads.
        let one = routes::all()
            .find(|r| r.kind == "policy.session")
            .expect("routed");
        let args = routes::args_of(one, Some("app-1"), &[]).expect("args");
        assert_eq!(named_session(&args).unwrap(), "app-1");
        // The socket dispatch reaches this part's builder.
        assert!(super::super::part_of("policy.sessions").is_some());
    }

    fn record(vault_id: &str, ns: &str, name: &str, roots: &[&str]) -> VaultRecord {
        VaultRecord {
            vault_id: vault_id.into(),
            ns: ns.into(),
            name: name.into(),
            sealed: false,
            roots: roots.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    /// Install a verified floor with vault-level entries for `vaults`.
    fn install_floor(hive_data: &std::path::Path, vaults: &[&str]) {
        use lean_ctx_hive::effective_floor::{
            FLOOR_FILE, FLOOR_SCHEMA, Floor, REQUIRED_FILE, floor_dir_in,
        };
        use lean_ctx_protocol::hive::effective_policy::{
            BundlePolicyV1, EffectivePolicyV1, POLICY_DOCUMENT_SCHEMA, VaultEffectivePolicyV1,
        };
        let effective = || EffectivePolicyV1 {
            schema: POLICY_DOCUMENT_SCHEMA.into(),
            security: Default::default(),
            context: Default::default(),
            sharing: Default::default(),
        };
        let floor = Floor {
            schema: FLOOR_SCHEMA.into(),
            version: 3,
            digest: format!("sha256:{}", "a".repeat(64)),
            not_after: "2999-01-01T00:00:00Z".into(),
            policy: BundlePolicyV1 {
                effective: effective(),
                vaults: vaults
                    .iter()
                    .map(|id| VaultEffectivePolicyV1 {
                        vault_id: (*id).into(),
                        effective: effective(),
                    })
                    .collect(),
            },
        };
        let dir = floor_dir_in(hive_data);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FLOOR_FILE), serde_json::to_vec(&floor).unwrap()).unwrap();
        std::fs::write(dir.join(REQUIRED_FILE), br#"{"version":3}"#).unwrap();
    }

    /// pp7: `installed` names the vaults it governs, with the checkouts
    /// linked to each, so a project is matched to its policy.
    #[test]
    fn installed_lists_each_vault_with_its_roots_and_which_policy_applies() {
        let dir = tempfile::tempdir().unwrap();
        install_floor(dir.path(), &["v_team", "v_elsewhere"]);
        let mut map = VaultMap::default();
        map.vaults.insert(
            "stable-api".into(),
            record("v_team", "platform", "api", &["/code/api-wt", "/code/api"]),
        );
        map.vaults.insert(
            "stable-web".into(),
            record("v_open", "platform", "web", &["/code/web"]),
        );
        map.save(dir.path()).unwrap();

        let doc = sessions_doc(dir.path(), chrono::Utc::now());
        let installed = doc
            .installed
            .as_ref()
            .expect("a verified policy is installed");
        assert_eq!(installed.version, Some(3));
        assert_eq!(installed.signature, "verified");
        assert_eq!(
            installed.vaults,
            vec![
                PolicyVault {
                    vault_id: "v_elsewhere".into(),
                    ns: None,
                    name: None,
                    roots: Vec::new(),
                    policy: "vault",
                },
                PolicyVault {
                    vault_id: "v_open".into(),
                    ns: Some("platform".into()),
                    name: Some("web".into()),
                    roots: vec!["/code/web".into()],
                    policy: "default",
                },
                PolicyVault {
                    vault_id: "v_team".into(),
                    ns: Some("platform".into()),
                    name: Some("api".into()),
                    roots: vec!["/code/api".into(), "/code/api-wt".into()],
                    policy: "vault",
                },
            ]
        );
        let text = super::super::document("policy.sessions", &doc);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["installed"]["vaults"][2]["vault_id"], "v_team",
            "{text}"
        );
        assert!(value["installed"]["vaults"][0]["ns"].is_null(), "{text}");
    }

    /// D4: `installed.enforcement` names the version enforced, when Hive
    /// acknowledged it, the live sessions under it and the bus report.
    #[test]
    fn installed_says_which_version_is_enforced_and_by_which_sessions() {
        use lean_ctx_hive::policy_sessions::{Reported, note_reported, record};
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now();
        install_floor(dir.path(), &[]);
        record(dir.path(), "app-live", "r1", now);
        record(
            dir.path(),
            "app-old",
            "r2",
            now - chrono::Duration::hours(1),
        );
        let enforcement = |doc: &SessionsDoc| {
            let text = super::super::document("policy.sessions", doc);
            serde_json::from_str::<serde_json::Value>(&text).unwrap()["installed"]["enforcement"]
                .clone()
        };
        assert_eq!(
            enforcement(&sessions_doc(dir.path(), now)),
            serde_json::json!({"applied_version": null, "acknowledged_at": null,
                "sessions": ["app-live"], "reported_at": null}),
            "verified and in force, not yet acknowledged by Hive"
        );
        let digest = format!("sha256:{}", "a".repeat(64));
        let state = serde_json::json!({"effective": {
            "version": 3, "digest": digest, "issued_at": "", "not_after": "",
            "key_id": "k_policy", "unenforced": [],
            "ack": {"version": 3, "digest": digest, "state": "acked",
                    "acked_at": "2026-11-05T11:00:00Z"}}});
        let policy_dir = dir.path().join("policy");
        std::fs::create_dir_all(&policy_dir).unwrap();
        std::fs::write(
            policy_dir.join("state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        note_reported(
            dir.path(),
            &Reported {
                version: 3,
                digest,
                sessions: 1,
                reported_at: "2026-11-05T11:00:05Z".into(),
            },
        );
        assert_eq!(
            enforcement(&sessions_doc(dir.path(), now)),
            serde_json::json!({"applied_version": 3,
                "acknowledged_at": "2026-11-05T11:00:00Z",
                "sessions": ["app-live"], "reported_at": "2026-11-05T11:00:05Z"})
        );
    }

    #[test]
    fn damaged_policy_files_mark_every_linked_vault_unverified() {
        let mut map = VaultMap::default();
        map.vaults
            .insert("s".into(), record("v1", "ns", "n", &["/code/a"]));
        let rows = policy_vaults(&map, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].policy, "unverified");
    }

    #[test]
    fn this_part_lists_its_kinds_and_routes_in_step() {
        let mut kinds: Vec<&str> = ROUTES.iter().map(|r| r.kind).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, KINDS);
        assert!(ROUTES.iter().all(|r| r.path.starts_with("/v1/local/hive/")));
    }
}
