// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud presence --json`: the workers of other machines on the
//! vault buses this machine follows, as the relay last recorded them
//! (`ctx_agent list` shows the same rows). No request leaves the machine.

use serde::Serialize;

use lean_ctx_cloud::CloudStore;
use lean_ctx_cloud::bus::presence::{LIST_WINDOW_HOURS, PresenceMap};
use lean_ctx_cloud::bus::v2::machine_name;

use super::Failure;

#[derive(Debug, Serialize)]
pub(crate) struct PresenceList {
    pub workers: Vec<WorkerRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct WorkerRow {
    pub worker_id: String,
    /// `agent` | `person`; `agent` when the Cloud did not say.
    pub kind: String,
    /// The worker's name this Instance knows (its own workers), else `null`.
    pub name: Option<String>,
    /// What a reader shows: the name the Cloud relayed, else `name`, else
    /// `null` (the reader falls back to the role, then the id).
    pub display_name: Option<String>,
    pub session_name: Option<String>,
    pub role: String,
    pub state: String,
    /// The machine the worker runs on, as the Cloud relayed it.
    pub instance: super::bus_presence::InstanceRow,
    /// The vault (`ns/name`) of the bus it was seen on; `root` is a path of
    /// another machine and stays `null`.
    pub project: super::bus_presence::ProjectRow,
    pub last_seen: String,
    pub address: String,
    /// Team presence (h7b): the linked account's name of the person the
    /// worker belongs to, as the Cloud relayed it, else `null`.
    pub person_name: Option<String>,
    /// The short name (first label, never a domain; an IP or a label with
    /// spaces stays whole) of the machine name the Cloud relayed, else of
    /// the Instance name (an older LeanCTX may relay a full host name),
    /// else `null`.
    pub machine_name: Option<String>,
    /// What the worker is working on (the status line its agent set,
    /// redacted by the sending Instance), else `null`.
    pub working_on: Option<String>,
}

/// The rows of `map` reported within the listing window before `now`,
/// newest first; `name_of` names the workers this Instance knows.
pub(crate) fn rows(
    map: &PresenceMap,
    now: lean_ctx_time::UtcDateTime,
    name_of: &dyn Fn(&str) -> Option<String>,
) -> Vec<WorkerRow> {
    map.remote_workers(now, lean_ctx_time::Duration::hours(LIST_WINDOW_HOURS))
        .into_iter()
        .map(|(worker, p)| WorkerRow {
            worker_id: worker.to_owned(),
            kind: p.names.kind.clone().unwrap_or_else(|| "agent".to_owned()),
            name: name_of(worker),
            display_name: p.names.display_name.clone().or_else(|| name_of(worker)),
            session_name: p.names.session_name.clone(),
            role: p.role.clone(),
            state: serde_json::to_value(p.state)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            instance: super::bus_presence::InstanceRow {
                id: p.names.instance_id.clone(),
                name: p.names.instance_name.clone(),
            },
            project: super::bus_presence::ProjectRow {
                root: None,
                vault: p.address.strip_prefix('#').map(str::to_owned),
            },
            last_seen: p.server_time.clone(),
            address: p.address.clone(),
            person_name: p.names.person_name.clone(),
            machine_name: p
                .names
                .machine_name
                .as_deref()
                .or(p.names.instance_name.as_deref())
                .and_then(machine_name),
            working_on: p.names.working_on.clone(),
        })
        .collect()
}

/// `cloud presence --json`, released like every Contracts kind.
pub(crate) fn run(args: &[String]) {
    super::kinds::run("cloud.presence", args);
}

pub(crate) fn build(_args: &[String]) -> Result<PresenceList, Failure> {
    let store = CloudStore::open().map_err(Failure::from)?;
    let own = store
        .instance()
        .ok()
        .flatten()
        .and_then(|i| lean_ctx_cloud::workers::attach::list(&store, &i.instance_id).ok())
        .unwrap_or_default();
    let name_of = |id: &str| {
        own.iter()
            .find(|w| w.worker_id == id)
            .and_then(|w| w.name.clone())
    };
    Ok(PresenceList {
        workers: rows(
            &PresenceMap::load(store.data_dir()),
            lean_ctx_time::UtcDateTime::now(),
            &name_of,
        ),
    })
}
