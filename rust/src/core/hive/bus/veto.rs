// SPDX-License-Identifier: Apache-2.0
//! A maintainer's veto of a host handoff or lock (SPEC-ENGINE §3.7.4, V9.5
//! phase A): a verified `control` frame with `action = veto` and a `ref` of
//! kind `lease` (a bus lock) or `handoff`.
//!
//! On receipt the host engine drops what it holds locally — the lock's
//! binding and the local lease behind it, or the open handoff offer — and
//! the holding agent's next `ctx_tasks`/`ctx_agent` call is refused once
//! with `claim_vetoed` ([`take_refusal`]). A `progress` `"vetoed: <reason>"`
//! goes to the place bus. Later references to the vetoed lock or handoff
//! are refused by [`super::control::veto_refusal`] while the veto stands.
//!
//! A control whose signature fails never gets here: `StateV2::apply_control`
//! ignores it and counts it `sig_invalid`.
//!
//! Kept in `<data>/hive/bus/vetoes.json` (pending notices per agent), under
//! its lock; shared by the relay (writer) and tool calls (readers).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::v2::{self, Control, ControlAction, MessageType, Publish};
use crate::core::hive::HiveError;
use crate::core::hive::state::{HiveStore, load_or_quarantine, lock_state, write_private};

const VETOES_FILE: &str = "vetoes.json";
/// A `progress` step is at most this long (C0 2.0 bus body).
const STEP_MAX: usize = 300;

/// The refusal an agent gets once after a veto took its lock or handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// `lease` or `handoff`.
    pub kind: String,
    pub ref_id: String,
    pub reason: String,
    pub control_id: String,
}

impl Notice {
    /// `claim_vetoed`, the maintainer's reason fenced as untrusted text.
    pub fn message(&self) -> String {
        format!(
            "claim_vetoed: A maintainer vetoed this claim ({} {}): {}",
            self.kind,
            self.ref_id,
            crate::core::hive::untrusted::wrap(
                crate::core::hive::untrusted::UntrustedKind::BusMessage,
                "maintainer",
                &self.reason,
            )
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Vetoes {
    /// Agent id → notices not yet delivered.
    #[serde(default)]
    pending: BTreeMap<String, Vec<Notice>>,
}

fn mutate<T>(data_dir: &Path, change: impl FnOnce(&mut Vetoes) -> T) -> Result<T, HiveError> {
    let dir = data_dir.join("bus");
    let _lock = lock_state(&dir, VETOES_FILE)?;
    let mut state = load_or_quarantine::<Vetoes>(&dir.join(VETOES_FILE), |_| true)
        .ok()
        .flatten()
        .unwrap_or_default();
    let before = state.clone();
    let out = change(&mut state);
    if state != before {
        let bytes = serde_json::to_vec(&state).map_err(|e| HiveError::Local(e.to_string()))?;
        write_private(&dir, VETOES_FILE, &bytes)?;
    }
    Ok(out)
}

/// What a veto did on this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Local agents whose lock or handoff it took.
    pub agents: Vec<String>,
    pub progress_queued: bool,
}

/// Apply a verified, issued (not lifted) veto. `worker_of` maps a local
/// agent id to its worker id on the place (for the `progress` post).
pub fn apply(
    store: &HiveStore,
    control: &Control,
    worker_of: &dyn Fn(&str) -> Option<String>,
) -> Result<Applied, HiveError> {
    let mut applied = Applied::default();
    let Some(target) = control.target_ref.as_ref() else {
        return Ok(applied);
    };
    if control.action != ControlAction::Veto {
        return Ok(applied);
    }
    let data = store.data_dir();
    match target.kind.as_str() {
        "lease" => {
            let local_refs = super::leases::Bindings::mutate(data, |b| {
                let refs: Vec<String> = b
                    .leases
                    .iter()
                    .filter(|(_, binding)| binding.lease_id == target.id)
                    .map(|(local, _)| local.clone())
                    .collect();
                for local in &refs {
                    b.leases.remove(local);
                }
                refs
            })?;
            for local in local_refs {
                if let Ok(Some(lease)) = crate::core::agent_lease::revoke_shared(&local) {
                    applied.agents.push(lease.request.owner_agent_id);
                }
            }
        }
        "handoff" => {
            let reason = "veto".to_string();
            let from = super::handoff::Handoffs::mutate(data, |h| {
                let mut from = Vec::new();
                for (id, offer) in &mut h.offers {
                    let named = *id == target.id || offer.entry_id == target.id;
                    if named
                        && matches!(
                            offer.state,
                            super::handoff::OfferState::Queued
                                | super::handoff::OfferState::Offered
                        )
                    {
                        offer.state = super::handoff::OfferState::Rejected {
                            by: "maintainer".into(),
                            reason: reason.clone(),
                        };
                        from.push(offer.from.clone());
                    }
                }
                from
            })?;
            applied.agents.extend(from);
        }
        _ => return Ok(applied),
    }
    applied.agents.sort();
    applied.agents.dedup();
    let notice = Notice {
        kind: target.kind.clone(),
        ref_id: target.id.clone(),
        reason: control.reason.clone(),
        control_id: control.id.clone(),
    };
    mutate(data, |v| {
        for agent in &applied.agents {
            let list = v.pending.entry(agent.clone()).or_default();
            if !list.iter().any(|n| n.control_id == notice.control_id) {
                list.push(notice.clone());
            }
        }
    })?;
    // `progress` "vetoed: <reason>" on the place bus, as the holding worker.
    let address = place_address(data, &control.place);
    let worker = applied.agents.iter().find_map(|a| worker_of(a));
    if let (Some(to), Some(worker_id)) = (address, worker) {
        let step: String = format!("vetoed: {}", control.reason)
            .chars()
            .take(STEP_MAX)
            .collect();
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default();
        super::relay_v2::queue_publish(
            store,
            Publish {
                schema: v2::BUS_SCHEMA.to_string(),
                client_msg_id: v2::new_ulid(now),
                to,
                kind: MessageType::Progress,
                worker_id,
                thread_id: None,
                reply_to: None,
                refs: Vec::new(),
                body: json!({ "step": step }),
                ck_wrapped: None,
                epoch: None,
                about: None,
                counts: None,
                window: None,
            },
        )?;
        applied.progress_queued = true;
    }
    Ok(applied)
}

/// `#ws/vault` of the linked vault behind a place ref `vault:<id>`.
fn place_address(data_dir: &Path, place: &str) -> Option<String> {
    let id = place.strip_prefix("vault:")?;
    crate::core::hive::vault::VaultMap::load(data_dir)
        .vaults
        .values()
        .find(|r| r.vault_id == id)
        .map(super::relay_v2::address_of)
}

/// The tools a veto notice answers: the claim and bus tools of §3.7.4.
fn answers(tool: &str) -> bool {
    matches!(tool, "ctx_tasks" | "ctx_agent")
}

/// The `claim_vetoed` refusal for `agent_id`'s next `ctx_tasks`/`ctx_agent`
/// call, delivered once. Missing or unreadable state never refuses.
pub fn take_refusal(agent_id: Option<&str>, tool: &str) -> Option<String> {
    let agent = agent_id?;
    if !answers(tool) {
        return None;
    }
    let store = HiveStore::open().ok()?;
    take_refusal_in(&store, agent)
}

pub(crate) fn take_refusal_in(store: &HiveStore, agent: &str) -> Option<String> {
    if !store.data_dir().join("bus").join(VETOES_FILE).exists() {
        return None;
    }
    mutate(store.data_dir(), |v| {
        let list = v.pending.get_mut(agent)?;
        let notice = (!list.is_empty()).then(|| list.remove(0));
        if list.is_empty() {
            v.pending.remove(agent);
        }
        notice
    })
    .ok()
    .flatten()
    .map(|n| n.message())
}

/// Whether a verified v2 veto stands for `ref_id` (a lock or handoff).
pub fn vetoed_v2(store: &HiveStore, ref_id: &str) -> Option<String> {
    let state = super::relay_v2::StateV2::load(store.data_dir());
    state.vetoed(ref_id).map(|c| {
        Notice {
            kind: c
                .target_ref
                .as_ref()
                .map_or_else(String::new, |r| r.kind.clone()),
            ref_id: ref_id.to_string(),
            reason: c.reason.clone(),
            control_id: c.id.clone(),
        }
        .message()
    })
}

#[cfg(test)]
#[path = "veto_tests.rs"]
mod tests;
