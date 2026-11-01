// SPDX-License-Identifier: Apache-2.0
//! Cross-instance leases through Hive (C5c).
//!
//! `ctx_agent lease_acquire` takes the local lease first (same-machine
//! contention needs no network), then asks Hive off the tool call
//! ([`acquire`], connector rule "Hive never blocks a tool call"): one
//! request of at most [`BUDGET`] in the in-process queue
//! ([`crate::core::hive::background`]), only while this machine is
//! enrolled with a Valid or Grace lease that has `bus.relay` and the project
//! is linked. Otherwise the answer is [`Outcome::Local`] at once, and the
//! agent is told the lease does not exclude other machines.
//!
//! Until Hive answered `granted` the Hive side is pending
//! ([`Outcome::Pending`]): no fencing token, nothing bound or announced.
//! What Hive answered (granted, held by another agent, vetoed, unreachable)
//! comes with the agent's next `ctx_agent` call, and a refusal releases the
//! local lease. A renewal or release needs Hive's `lease_id`, so it is
//! queued behind the grant. `POST /v1/bus/leases` is idempotent for the
//! holder (a re-acquire extends the same lease and token).
//!
//! The Hive side of a local lease is kept in `<data>/hive/bus/leases.json`
//! (beside, not inside, the machine-wide lease store, whose schema other
//! processes read strictly). Releases are queued there and sent by the relay;
//! a lost release is covered by the service's own expiry. Tool calls and the
//! relay change it only through [`Bindings::mutate`], under `leases.json.lock`,
//! and never hold that lock across a request (RB8 E5): read, ask Hive,
//! then apply the answer to what is on disk by then.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::config::{HiveBusMode, HiveConfig};
use crate::core::hive::state::{HiveStore, load_or_quarantine, lock_state, write_private};
use crate::core::hive::vault::VaultMap;
use lean_ctx_protocol::hive::bus::{
    BUS_LEASE_RENEW_REQUEST_SCHEMA, BUS_LEASE_REQUEST_SCHEMA, BusLeaseV1, MAX_BUS_LEASE_MS,
};
use lean_ctx_protocol::hive::lease::{HiveRight, LeaseState};

/// Whole budget of one acquire or renew a tool call queued.
pub const BUDGET: Duration = Duration::from_secs(3);
const LEASES_FILE: &str = "leases.json";
/// Queued releases kept at most; the oldest go first, the service lets
/// their leases expire (RB8 M14).
const MAX_RELEASES: usize = 256;
/// A binding this long past its expiry is dropped: Hive forgot it too.
const STALE_BINDING: chrono::Duration = chrono::Duration::days(1);

/// What Hive answered, or why it was not asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Granted {
        lease_id: String,
        fencing_token: u64,
        expires_at: String,
    },
    /// Another agent (possibly on another machine) holds it.
    Held {
        agent_id: String,
        instance_id: String,
        expires_at: String,
    },
    /// An organization veto bars this agent from the resource.
    Vetoed,
    /// Hive no longer knows the lease (expired on the service clock or superseded).
    Superseded,
    /// Hive was not asked or did not answer in time: local only.
    Local(String),
    /// Hive is being asked off the tool call; its answer comes with the
    /// agent's next `ctx_agent` call. No fencing token yet.
    Pending,
}

/// Local lease refs whose Hive acquire is queued or running in this
/// process: their renewal and release wait behind it.
static PENDING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn pending() -> MutexGuard<'static, BTreeSet<String>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The queue caller of `agent_id`'s `ctx_agent` calls
/// ([`crate::core::hive::background::with_answers`]).
pub fn caller(agent_id: &str) -> String {
    format!("ctx_agent:{agent_id}")
}

/// The Hive lease behind a local one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub lease_id: String,
    pub fencing_token: u64,
    /// On disk as `project_id` (pre-VA name) for downgrades and mixed-version teams.
    #[serde(rename = "project_id", alias = "vault_id")]
    pub vault_id: String,
    pub expires_at: String,
}

/// A release still to send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub lease_id: String,
    pub fencing_token: u64,
}

/// `leases.json`: local `lease_ref` → Hive lease, plus queued releases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bindings {
    #[serde(default)]
    pub leases: BTreeMap<String, Binding>,
    #[serde(default)]
    pub releases: Vec<Release>,
}

impl Bindings {
    /// The bindings on disk. A damaged file is moved aside (RB8 M12) and the
    /// bindings start empty: the service expires what this machine forgot.
    pub fn load(data_dir: &Path) -> Self {
        load_or_quarantine(&data_dir.join("bus").join(LEASES_FILE), |_: &Self| true)
            .unwrap_or_else(|error| {
                tracing::warn!("hive lease bindings: {error}");
                None
            })
            .unwrap_or_default()
    }

    fn save(&self, data_dir: &Path) -> Result<(), HiveError> {
        let bytes = serde_json::to_vec(self).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(&data_dir.join("bus"), LEASES_FILE, &bytes)
    }

    /// Load, change and save under the bindings lock (RB8 E5). Stale
    /// bindings and releases past `MAX_RELEASES` go (M14). Never call it
    /// around a request.
    pub fn mutate<T>(data_dir: &Path, change: impl FnOnce(&mut Self) -> T) -> Result<T, HiveError> {
        let _lock = lock_state(&data_dir.join("bus"), LEASES_FILE)?;
        let mut bindings = Self::load(data_dir);
        let before = bindings.clone();
        let out = change(&mut bindings);
        bindings.prune(chrono::Utc::now());
        if bindings != before {
            bindings.save(data_dir)?;
        }
        Ok(out)
    }

    fn prune(&mut self, now: chrono::DateTime<chrono::Utc>) {
        self.leases.retain(|_, b| {
            chrono::DateTime::parse_from_rfc3339(&b.expires_at)
                .map_or(true, |at| at.to_utc() + STALE_BINDING > now)
        });
        let excess = self.releases.len().saturating_sub(MAX_RELEASES);
        self.releases.drain(..excess);
    }
}

/// The machine-independent Hive `resource_ref` of a lease target in a
/// project: no path leaves the engine, and every checkout of the project
/// names a file the same way.
pub fn resource_ref(vault_id: &str, kind: &str, target: &str) -> String {
    let digest = Sha256::digest(
        format!("leanctx.hive.bus-lease.v1\n{vault_id}\n{kind}\n{target}").as_bytes(),
    );
    hex::encode(digest)
}

/// `(kind, normalized target)` of a `ctx_agent` lease target: `symbol:<name>`
/// or a path inside `project_root` (repo-relative, `/`-separated). `None`
/// for a path outside the project.
pub fn normalize_target(project_root: &Path, raw: &str) -> Option<(&'static str, String)> {
    let raw = raw.trim();
    if let Some(symbol) = raw.strip_prefix("symbol:") {
        let symbol = symbol.trim();
        return (!symbol.is_empty()).then(|| ("symbol", symbol.to_string()));
    }
    let path = Path::new(raw);
    // Anchored, not only `is_absolute`: on Windows a rooted path without a
    // drive (`/home/ada/x`, as another checkout reports it) is not absolute,
    // yet it names a place below its root, never a relative target.
    let anchored = path.has_root()
        || matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        );
    let relative = if anchored {
        path.strip_prefix(project_root).ok()?
    } else {
        path
    };
    let mut parts: Vec<String> = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| ("file", parts.join("/")))
}

/// A connected machine and the Hive project of a local root.
pub(super) struct Connected {
    pub(super) store: HiveStore,
    pub(super) session: Session,
    pub(super) vault_id: String,
}

/// Whether Hive applies to `project_root`; `Err` says why not (no request made).
pub(super) fn connected(project_root: &Path) -> Result<Connected, String> {
    let config = crate::core::config::Config::load().hive.clone();
    connected_with(
        &config,
        HiveStore::open().map_err(|e| e.to_string())?,
        project_root,
    )
}

fn connected_with(
    config: &HiveConfig,
    store: HiveStore,
    project_root: &Path,
) -> Result<Connected, String> {
    if config.bus == HiveBusMode::Off || config.enabled == Some(false) {
        return Err("the Hive bus is off".into());
    }
    let instance = store
        .instance()
        .ok()
        .flatten()
        .ok_or_else(|| "this machine is not connected to Hive".to_string())?;
    let root = project_root.to_string_lossy();
    let vault_id = VaultMap::load(store.data_dir())
        .for_root(&root)
        .map(|p| p.vault_id.clone())
        .ok_or_else(|| "this repository is not linked to a vault".to_string())?;
    let session = config
        .root_key(&instance.url)
        .and_then(|root| Session::load(store.clone(), root))
        .map_err(|e| e.to_string())?;
    if !matches!(
        session.lease.state_at(chrono::Utc::now()),
        LeaseState::Valid | LeaseState::Grace
    ) {
        return Err("the Hive lease has lapsed".into());
    }
    if !session.lease.lease.grants(HiveRight::BusRelay) {
        return Err("the Hive lease does not include the bus".into());
    }
    Ok(Connected {
        store,
        session,
        vault_id,
    })
}

fn decode_lease(value: serde_json::Value) -> Result<BusLeaseV1, HiveError> {
    let lease: BusLeaseV1 = serde_json::from_value(value)
        .map_err(|e| HiveError::Protocol(format!("bus lease: {e}")))?;
    lease
        .validate()
        .map_err(|e| HiveError::Protocol(format!("bus lease: {e}")))?;
    Ok(lease)
}

fn outcome(answer: Result<BusLeaseV1, HiveError>) -> Outcome {
    match answer {
        Ok(BusLeaseV1 {
            granted: true,
            lease_id: Some(lease_id),
            fencing_token: Some(fencing_token),
            expires_at: Some(expires_at),
            ..
        }) => Outcome::Granted {
            lease_id,
            fencing_token,
            expires_at: expires_at.as_str().to_string(),
        },
        Ok(BusLeaseV1 {
            holder: Some(holder),
            ..
        }) => Outcome::Held {
            agent_id: holder.agent_id,
            instance_id: holder.instance_id,
            expires_at: holder.expires_at.as_str().to_string(),
        },
        Ok(_) => Outcome::Local("Hive sent an incomplete lease".into()),
        Err(HiveError::Vetoed) => Outcome::Vetoed,
        Err(HiveError::NotFound) => Outcome::Superseded,
        Err(HiveError::Offline(why)) => Outcome::Local(format!("Hive unreachable ({why})")),
        Err(error) => Outcome::Local(error.to_string()),
    }
}

/// Ask Hive, off the tool call, for the lease the agent already holds
/// locally (`local_ref`). [`Outcome::Local`] at once when Hive does not
/// apply (no request); otherwise [`Outcome::Pending`], and `settle` runs in
/// the queue with Hive's answer and returns the line the agent gets with
/// its next `ctx_agent` call. A grant is bound to `local_ref` before
/// `settle` runs; `settle` releases the local lease on a refusal.
pub fn acquire(
    project_root: &Path,
    agent_id: &str,
    target: &str,
    duration_ms: u64,
    local_ref: &str,
    settle: impl FnOnce(Outcome) -> Option<String> + Send + 'static,
) -> Outcome {
    let connected = match connected(project_root) {
        Ok(connected) => connected,
        Err(why) => return Outcome::Local(why),
    };
    if normalize_target(project_root, target).is_none() {
        return Outcome::Local("the target is outside the project".into());
    }
    let (root, agent, target, local) = (
        project_root.to_path_buf(),
        agent_id.to_string(),
        target.to_string(),
        local_ref.to_string(),
    );
    pending().insert(local.clone());
    crate::core::hive::background::submit(&caller(agent_id), move || {
        let outcome = acquire_with(&connected, &root, &agent, &target, duration_ms, &local);
        pending().remove(&local);
        settle(outcome)
    });
    Outcome::Pending
}

fn acquire_with(
    c: &Connected,
    project_root: &Path,
    agent_id: &str,
    target: &str,
    duration_ms: u64,
    local_ref: &str,
) -> Outcome {
    let Some((kind, normalized)) = normalize_target(project_root, target) else {
        return Outcome::Local("the target is outside the project".into());
    };
    let deadline = Instant::now() + BUDGET;
    let body = json!({
        "schema": BUS_LEASE_REQUEST_SCHEMA,
        "resource_kind": kind,
        "resource_ref": resource_ref(&c.vault_id, kind, &normalized),
        "vault_id": c.vault_id,
        "owner_agent_id": agent_id,
        "duration_ms": duration_ms.clamp(1, MAX_BUS_LEASE_MS),
    });
    let answer = c
        .session
        .bounded_client(deadline)
        .and_then(|client| client.post("/bus/leases", &body))
        .and_then(decode_lease);
    let result = outcome(answer);
    if let Outcome::Granted {
        lease_id,
        fencing_token,
        expires_at,
    } = &result
    {
        let binding = Binding {
            lease_id: lease_id.clone(),
            fencing_token: *fencing_token,
            vault_id: c.vault_id.clone(),
            expires_at: expires_at.clone(),
        };
        if let Err(error) = Bindings::mutate(c.store.data_dir(), |b| {
            b.leases.insert(local_ref.to_string(), binding);
        }) {
            // Unrecorded, the lease would never be renewed or released by us;
            // the service lets it expire.
            return Outcome::Local(format!("could not record the Hive lease: {error}"));
        }
    }
    result
}

/// Renew the Hive lease bound to `local_ref`, off the tool call and behind
/// a pending acquire (it needs Hive's `lease_id`). A local-only lease gets
/// [`Outcome::Local`] at once; otherwise [`Outcome::Pending`], and `settle`
/// gets Hive's answer as [`acquire`]'s does.
pub fn renew(
    project_root: &Path,
    agent_id: &str,
    local_ref: &str,
    duration_ms: u64,
    settle: impl FnOnce(Outcome) -> Option<String> + Send + 'static,
) -> Outcome {
    let Ok(store) = HiveStore::open() else {
        return Outcome::Local("no Hive state".into());
    };
    let bound = Bindings::load(store.data_dir())
        .leases
        .contains_key(local_ref);
    if !bound && !pending().contains(local_ref) {
        return Outcome::Local("this lease is local only".into());
    }
    let connected = match connected(project_root) {
        Ok(connected) => connected,
        Err(why) => return Outcome::Local(why),
    };
    let local = local_ref.to_string();
    crate::core::hive::background::submit(&caller(agent_id), move || {
        settle(renew_with(&connected, &local, duration_ms))
    });
    Outcome::Pending
}

fn renew_with(c: &Connected, local_ref: &str, duration_ms: u64) -> Outcome {
    let data = c.store.data_dir();
    let Some(binding) = Bindings::load(data).leases.get(local_ref).cloned() else {
        return Outcome::Local("Hive did not grant this lease".into());
    };
    let deadline = Instant::now() + BUDGET;
    let body = json!({
        "schema": BUS_LEASE_RENEW_REQUEST_SCHEMA,
        "fencing_token": binding.fencing_token,
        "duration_ms": duration_ms.clamp(1, MAX_BUS_LEASE_MS),
    });
    let path = format!("/bus/leases/{}/renew", binding.lease_id);
    let result = outcome(
        c.session
            .bounded_client(deadline)
            .and_then(|client| client.post(&path, &body))
            .and_then(decode_lease),
    );
    // Applied to the binding as it is now, and only while it is still the
    // lease that was renewed (a release or a new grant may have come since).
    let applied = Bindings::mutate(data, |bindings| {
        let same = |b: &Binding| b.lease_id == binding.lease_id;
        match &result {
            Outcome::Granted { expires_at, .. } => {
                if let Some(b) = bindings.leases.get_mut(local_ref).filter(|b| same(b)) {
                    b.expires_at.clone_from(expires_at);
                }
            }
            Outcome::Superseded | Outcome::Vetoed
                if bindings.leases.get(local_ref).is_some_and(same) =>
            {
                bindings.leases.remove(local_ref);
            }
            _ => {}
        }
    });
    if let Err(error) = applied {
        tracing::debug!("hive lease bindings not saved: {error}");
    }
    result
}

/// The Hive lease bound to `local_ref`, if any.
pub fn binding(local_ref: &str) -> Option<Binding> {
    let store = HiveStore::open().ok()?;
    Bindings::load(store.data_dir())
        .leases
        .get(local_ref)
        .cloned()
}

/// Queue the release of the Hive lease bound to `local_ref` (the relay
/// sends it). No network in the tool call; while the acquire is still
/// pending, the release is queued behind it and applies to its grant.
pub fn release(agent_id: &str, local_ref: &str) {
    let Ok(store) = HiveStore::open() else {
        return;
    };
    // Checked first: a pending acquire that ends meanwhile has bound its
    // grant before it leaves `PENDING`, so one of the two releases sees it.
    let was_pending = pending().contains(local_ref);
    queue_release_in(store.data_dir(), local_ref);
    if was_pending {
        let (data, local) = (store.data_dir().to_path_buf(), local_ref.to_string());
        crate::core::hive::background::submit(&caller(agent_id), move || {
            queue_release_in(&data, &local);
            None
        });
    }
}

fn queue_release_in(data_dir: &Path, local_ref: &str) {
    let queued = Bindings::mutate(data_dir, |bindings| {
        if let Some(b) = bindings.leases.remove(local_ref) {
            bindings.releases.push(Release {
                lease_id: b.lease_id,
                fencing_token: b.fencing_token,
            });
        }
    });
    if let Err(error) = queued {
        tracing::debug!("hive lease release not queued: {error}");
    }
}

/// The relay's half: send queued releases (blocking; run off the socket loop).
/// A release Hive no longer knows counts as done.
pub fn send_releases(store: &HiveStore, session: &Session) {
    let data = store.data_dir();
    // Sent without the lock; only the ones done leave the queue as it is by
    // then, so a release queued meanwhile stays (RB8 E5).
    let done: Vec<Release> = Bindings::load(data)
        .releases
        .into_iter()
        .filter(|r| {
            let path = format!(
                "/bus/leases/{}?fencing_token={}",
                r.lease_id, r.fencing_token
            );
            match session.with_client(|client| client.delete(&path)) {
                Ok(()) | Err(HiveError::NotFound) => true,
                Err(error) => matches!(
                    error,
                    HiveError::Forbidden | HiveError::Vetoed | HiveError::Protocol(_)
                ),
            }
        })
        .collect();
    if done.is_empty() {
        return;
    }
    if let Err(error) = Bindings::mutate(data, |b| b.releases.retain(|r| !done.contains(r))) {
        tracing::debug!("hive lease releases not saved: {error}");
    }
}

/// One line for `ctx_agent`: where the lease holds. The Hive fencing token
/// stays in `leases.json` (0600) and is never printed, like a task claim's.
pub fn scope_line(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Granted { .. } | Outcome::Held { .. } | Outcome::Vetoed | Outcome::Superseded => {
            "scope=hive".into()
        }
        Outcome::Local(why) => {
            format!("scope=local ({why}; other machines are not excluded)")
        }
        Outcome::Pending => "scope=pending (Hive is asked off this call; until it grants, \
             other machines are not excluded; its answer comes with your next ctx_agent call)"
            .into(),
    }
}

#[cfg(test)]
mod vault_rename_tests {
    use super::*;

    /// `Binding` of the engine before the VA rename.
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct PreVaBinding {
        lease_id: String,
        fencing_token: u64,
        project_id: String,
        expires_at: String,
    }

    #[test]
    fn lease_bindings_keep_pre_va_names() {
        let old = r#"{"lease_id":"lse_1","fencing_token":4,"project_id":"v1","expires_at":"2026-10-22T10:00:00Z"}"#;
        let read = crate::core::hive::vault::assert_pre_va_round_trip::<Binding, PreVaBinding>(
            old,
            "project_id",
            "vault_id",
        );
        assert_eq!((read.vault_id.as_str(), read.fencing_token), ("v1", 4));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_checkout_names_a_file_the_same_way() {
        let a = Path::new("/home/ada/src/payments");
        let b = Path::new("/Users/bob/work/payments-clone");
        let (kind, ta) = normalize_target(a, "/home/ada/src/payments/src/lib.rs").unwrap();
        let (_, tb) = normalize_target(b, "./src/../src/lib.rs").unwrap();
        assert_eq!((kind, ta.as_str()), ("file", "src/lib.rs"));
        assert_eq!(ta, tb);
        let pid = "0123456789abcdef0123456789abcdef";
        let ref_a = resource_ref(pid, kind, &ta);
        assert_eq!(ref_a, resource_ref(pid, kind, &tb));
        assert_eq!(ref_a.len(), 64);
        assert!(!ref_a.contains("lib"));
        assert_ne!(
            ref_a,
            resource_ref("fedcba9876543210fedcba9876543210", kind, &ta)
        );
        assert_ne!(ref_a, resource_ref(pid, "symbol", &ta));
        assert_eq!(
            normalize_target(a, "symbol: Parser::parse"),
            Some(("symbol", "Parser::parse".into()))
        );
        assert_eq!(
            normalize_target(a, "/etc/passwd"),
            None,
            "outside the project"
        );
        assert_eq!(normalize_target(a, "../other/x.rs"), None);
        assert_eq!(normalize_target(a, "symbol:"), None);
    }

    #[test]
    fn answers_map_to_outcomes_and_the_scope_is_always_named() {
        let granted = serde_json::json!({"schema": "leanctx.hive.bus-lease/v1", "granted": true,
            "lease_id": "bl_1", "fencing_token": 4, "expires_at": "2026-10-21T12:10:00Z",
            "server_time": "2026-10-21T12:00:00Z"});
        let out = outcome(decode_lease(granted));
        assert!(matches!(
            out,
            Outcome::Granted {
                fencing_token: 4,
                ..
            }
        ));
        assert_eq!(scope_line(&out), "scope=hive", "the token is never printed");
        let held = serde_json::json!({"schema": "leanctx.hive.bus-lease/v1", "granted": false,
            "holder": {"agent_id": "beta", "instance_id": "inst_b", "expires_at": "2026-10-21T12:10:00Z"},
            "server_time": "2026-10-21T12:00:00Z"});
        assert!(
            matches!(outcome(decode_lease(held)), Outcome::Held { agent_id, .. } if agent_id == "beta")
        );
        assert_eq!(outcome(Err(HiveError::Vetoed)), Outcome::Vetoed);
        assert_eq!(outcome(Err(HiveError::NotFound)), Outcome::Superseded);
        let offline = outcome(Err(HiveError::Offline("timeout".into())));
        assert!(scope_line(&offline).starts_with("scope=local (Hive unreachable (timeout)"));
        assert!(scope_line(&offline).ends_with("other machines are not excluded)"));
        let pending = scope_line(&Outcome::Pending);
        assert!(pending.starts_with("scope=pending (") && !pending.contains("fencing"));
    }

    #[test]
    fn without_an_enrollment_no_request_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let err = connected_with(&HiveConfig::default(), store.clone(), dir.path())
            .err()
            .unwrap();
        assert_eq!(err, "this machine is not connected to Hive");
        let off = HiveConfig {
            bus: HiveBusMode::Off,
            ..HiveConfig::default()
        };
        assert_eq!(
            connected_with(&off, store, dir.path()).err().unwrap(),
            "the Hive bus is off"
        );
    }

    #[test]
    fn bindings_round_trip_and_queue_releases() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = Bindings::default();
        b.leases.insert(
            "lease-ref:1".into(),
            Binding {
                lease_id: "bl_1".into(),
                fencing_token: 2,
                vault_id: "p".into(),
                expires_at: "2026-10-21T12:10:00Z".into(),
            },
        );
        b.save(dir.path()).unwrap();
        assert_eq!(Bindings::load(dir.path()), b);
    }
}
