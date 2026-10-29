// SPDX-License-Identifier: Apache-2.0
//! Conductor of a swarm, engine side (SPEC-ENGINE §3.13 "Conductor", V7.5;
//! CONTRACT §3.20.4, RESOLUTIONS R107).
//!
//! Only the swarm owner's conductor workers and named delegates conduct.
//! The engine offers conductor actions only when the server's role says so:
//! the participant worker is listed in E22 `swarm.conductor_workers` or
//! `swarm.delegates`. Every call carries the instance token of the worker's
//! own instance and the worker id; the server refuses a worker of another
//! instance (`403 forbidden reason: worker_not_owned`) and an ineligible
//! one (`403 forbidden reason: not_conductor`).
//!
//! E28: `POST /swarms/{id}/conductor/lease {worker_id}` → `{epoch,
//! expires_at, holder}` (10 min; `409 conductor_busy` with the holder),
//! `POST …/lease/renew {worker_id, epoch}` (every ≤ 5 min; `409
//! stale_epoch`), `DELETE …/lease?epoch=&worker_id=`. E29: `POST
//! /swarms/{id}/conductor/actions {worker_id, epoch, action, …}` with the
//! actions `split`, `assign`, `synthesize`, `block`, `unblock`, `drop`,
//! `reopen`, `edit`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::HiveError;
use super::convert::ConvertApi;

/// Children of one `split` (C0 `split{children[1..50]}`).
pub const MAX_SPLIT_CHILDREN: usize = 50;

/// Whether `worker_id` may conduct the swarm an E22 goal answer describes.
pub fn eligible(goal: &Value, worker_id: &str) -> bool {
    ["conductor_workers", "delegates"].iter().any(|list| {
        goal["swarm"][list]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(worker_id)))
    })
}

/// The conductor lease this worker holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Lease {
    pub epoch: u64,
    pub expires_at: u64,
    pub holder: Holder,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Holder {
    pub worker_id: String,
    pub name: String,
}

/// One child goal of a `split`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub title: String,
    pub text: String,
    pub acceptance: Vec<String>,
    /// `subgoal` or `task`.
    pub kind: String,
}

/// An E29 action (C0 `routes/conductor_action.*.request.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Split {
        parent: String,
        children: Vec<Child>,
    },
    Assign {
        goal: String,
        assignee: String,
    },
    Synthesize {
        parent: String,
        title: String,
    },
    Block {
        goal: String,
        reason: String,
    },
    Unblock {
        goal: String,
    },
    Drop {
        goal: String,
        reason: String,
    },
    Reopen {
        goal: String,
        reason: String,
    },
    Edit {
        goal: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        acceptance: Option<Vec<String>>,
    },
}

/// Why a conductor call did not go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Checked locally before any call: the role does not allow it.
    NotConductor,
    /// The worker belongs to another instance.
    WorkerNotOwned,
    /// Another conductor holds the lease.
    Busy { holder: Option<String> },
    /// The lease moved on: take it again.
    StaleEpoch,
    /// Paused or archived swarm, or another refusal (`code`/`reason`).
    Other { status: u16, code: String },
}

impl Refusal {
    pub fn code(&self) -> &str {
        match self {
            Self::NotConductor => "not_conductor",
            Self::WorkerNotOwned => "worker_not_owned",
            Self::Busy { .. } => "conductor_busy",
            Self::StaleEpoch => "stale_epoch",
            Self::Other { code, .. } => code,
        }
    }

    fn of(status: u16, body: &Value) -> Self {
        let e = &body["error"];
        let reason = e["reason"].as_str().unwrap_or_default();
        let code = e["code"].as_str().unwrap_or_default();
        match (status, code, reason) {
            (403, _, "not_conductor") => Self::NotConductor,
            (403, _, "worker_not_owned") => Self::WorkerNotOwned,
            (409, _, "conductor_busy") => Self::Busy {
                holder: e["holder"]["worker_id"].as_str().map(str::to_string),
            },
            (409, "stale_epoch", _) => Self::StaleEpoch,
            _ => Self::Other {
                status,
                code: if reason.is_empty() { code } else { reason }.to_string(),
            },
        }
    }
}

impl From<Refusal> for HiveError {
    fn from(r: Refusal) -> Self {
        HiveError::Refused(format!("conductor: {}", r.code()))
    }
}

/// A conductor session of one participant worker in one swarm.
pub struct Conductor<'a> {
    pub api: &'a dyn ConvertApi,
    pub swarm_id: &'a str,
    pub worker_id: &'a str,
    /// From E22: the role the server gave this worker.
    pub eligible: bool,
}

fn ok_or(status: u16, body: Value) -> Result<Value, Result<Refusal, HiveError>> {
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(Ok(Refusal::of(status, &body)))
    }
}

/// `Err(Ok(refusal))` for a contract refusal, `Err(Err(_))` for transport
/// or shape errors.
pub type Answer<T> = Result<T, Result<Refusal, HiveError>>;

impl Conductor<'_> {
    fn gate(&self) -> Answer<()> {
        if self.eligible {
            Ok(())
        } else {
            // No conductor action for a worker the server does not list.
            Err(Ok(Refusal::NotConductor))
        }
    }

    fn base(&self) -> String {
        format!("/swarms/{}/conductor", self.swarm_id)
    }

    fn lease_from(body: Value) -> Answer<Lease> {
        serde_json::from_value(body)
            .map_err(|e| Err(HiveError::Protocol(format!("conductor lease: {e}"))))
    }

    /// E28 acquire.
    pub fn acquire(&self) -> Answer<Lease> {
        self.gate()?;
        let (status, body) = self
            .api
            .answer(
                "POST",
                &format!("{}/lease", self.base()),
                Some(&json!({ "worker_id": self.worker_id })),
            )
            .map_err(Err)?;
        Self::lease_from(ok_or(status, body)?)
    }

    /// E28 renew (every ≤ 5 min).
    pub fn renew(&self, epoch: u64) -> Answer<Lease> {
        self.gate()?;
        let (status, body) = self
            .api
            .answer(
                "POST",
                &format!("{}/lease/renew", self.base()),
                Some(&json!({ "worker_id": self.worker_id, "epoch": epoch })),
            )
            .map_err(Err)?;
        Self::lease_from(ok_or(status, body)?)
    }

    /// E28 release.
    pub fn release(&self, epoch: u64) -> Answer<()> {
        self.gate()?;
        let (status, body) = self
            .api
            .answer(
                "DELETE",
                &format!(
                    "{}/lease?epoch={epoch}&worker_id={}",
                    self.base(),
                    self.worker_id
                ),
                None,
            )
            .map_err(Err)?;
        ok_or(status, body).map(|_| ())
    }

    /// The E29 body of `action` under the lease `epoch`.
    pub fn action_body(&self, epoch: u64, action: &Action) -> Result<Value, HiveError> {
        if let Action::Split { children, .. } = action
            && !(1..=MAX_SPLIT_CHILDREN).contains(&children.len())
        {
            return Err(HiveError::Config(format!(
                "a split has 1 to {MAX_SPLIT_CHILDREN} children"
            )));
        }
        let mut body = serde_json::to_value(action).map_err(|e| HiveError::Local(e.to_string()))?;
        body["worker_id"] = self.worker_id.into();
        body["epoch"] = epoch.into();
        Ok(body)
    }

    /// E29: one action; the answer carries the changed goal nodes and the
    /// swarm log seqs it appended.
    pub fn act(&self, epoch: u64, action: &Action) -> Answer<Value> {
        self.gate()?;
        let body = self.action_body(epoch, action).map_err(Err)?;
        let (status, answer) = self
            .api
            .answer("POST", &format!("{}/actions", self.base()), Some(&body))
            .map_err(Err)?;
        ok_or(status, answer)
    }
}

/// `lean-ctx hive swarm plan <swarm> --worker <wkr_…> lease | renew <epoch>
/// | release <epoch> | act <epoch> --action '<json>'` (SPEC-ENGINE §3.18
/// entry point, run by the person in a terminal). The role is read with
/// E22 first; a worker the server does not list gets no conductor action.
pub fn cli(
    session: &crate::core::hive::auth::Session,
    args: &[String],
) -> Result<String, HiveError> {
    let usage = || {
        HiveError::Config(
            "usage: lean-ctx hive swarm plan <swarm> --worker <wkr_…> lease | renew <epoch> | release <epoch> | act <epoch> --action '<json>'"
                .into(),
        )
    };
    let mut positional = Vec::new();
    let (mut worker, mut action) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--worker" => worker = it.next().cloned(),
            "--action" => action = it.next().cloned(),
            _ => positional.push(a.clone()),
        }
    }
    let (Some(swarm), Some(worker)) = (positional.first(), worker) else {
        return Err(usage());
    };
    let epoch = |i: usize| -> Result<u64, HiveError> {
        positional
            .get(i)
            .and_then(|e| e.parse().ok())
            .ok_or_else(usage)
    };
    let goal =
        session.with_client(|c| c.get(&format!("/swarms/{swarm}/goal?worker_id={worker}")))?;
    let c = Conductor {
        api: session,
        swarm_id: swarm,
        worker_id: &worker,
        eligible: eligible(&goal, &worker),
    };
    let flat = |r: Result<Refusal, HiveError>| match r {
        Ok(refusal) => HiveError::from(refusal),
        Err(e) => e,
    };
    match positional.get(1).map(String::as_str) {
        Some("lease") => {
            let l = c.acquire().map_err(flat)?;
            Ok(format!(
                "Conductor lease epoch {} until {} (renew within 5 min)",
                l.epoch, l.expires_at
            ))
        }
        Some("renew") => {
            let l = c.renew(epoch(2)?).map_err(flat)?;
            Ok(format!(
                "Conductor lease epoch {} until {}",
                l.epoch, l.expires_at
            ))
        }
        Some("release") => {
            c.release(epoch(2)?).map_err(flat)?;
            Ok("Conductor lease released".into())
        }
        Some("act") => {
            let action: Action = serde_json::from_str(action.as_deref().ok_or_else(usage)?)
                .map_err(|e| HiveError::Config(format!("--action: {e}")))?;
            let answer = c.act(epoch(2)?, &action).map_err(flat)?;
            Ok(format!(
                "Done: {} goal node(s) changed, swarm log seq(s) {}",
                answer["nodes"].as_array().map_or(0, Vec::len),
                answer["seqs"]
            ))
        }
        _ => Err(usage()),
    }
}

#[cfg(test)]
#[path = "conductor_tests.rs"]
mod tests;
