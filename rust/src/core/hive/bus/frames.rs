// SPDX-License-Identifier: Apache-2.0
//! What the relay does with each service frame, and what it sends besides
//! publishes on each tick. `ws.rs` keeps only the socket loop.

use std::collections::HashMap;

use super::handoff::{self, Handoffs};
use super::messages;
use super::presence::PresenceMap;
use super::relay::{RelayState, apply_delivery};
use crate::core::agents::AgentRegistry;
use crate::core::hive::state::HiveStore;
use crate::core::hive::vault::VaultMap;
use lean_ctx_protocol::hive::bus::BusFrameV1;

/// Apply one service frame; returns the frame to answer with, if any.
pub(super) fn apply(
    frame: &BusFrameV1,
    store: &HiveStore,
    map: &VaultMap,
    state: &mut RelayState,
    pending: &mut HashMap<String, String>,
) -> Option<BusFrameV1> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let data = store.data_dir();
    match frame {
        BusFrameV1::Published { client_msg_id, .. } => {
            if let Some(entry) = pending.remove(client_msg_id) {
                state.mark_forwarded(&entry);
            }
            store.record_success("bus", &now);
            None
        }
        BusFrameV1::MessageStored(stored) => {
            messages::stored(data, stored);
            store.record_success("bus", &now);
            None
        }
        BusFrameV1::MessageDeliver(delivery) => {
            store.record_success("bus", &now);
            messages::receive(data, delivery)
        }
        BusFrameV1::MessageStatus(status) => {
            if let Err(error) = messages::status(data, status) {
                store.record_error(&format!("bus message status: {error}"));
            }
            None
        }
        BusFrameV1::TaskEvent(event) => {
            if let Err(error) = messages::task_event(data, event) {
                store.record_error(&format!("bus task event: {error}"));
            }
            None
        }
        BusFrameV1::Deliver { .. } => {
            let applied = AgentRegistry::mutate_locked(|registry| {
                apply_delivery(registry, map, state, frame)
            });
            match applied {
                Ok((_, Ok(Some((channel, seq))))) => {
                    store.record_success("bus", &now);
                    Some(BusFrameV1::Ack { channel, seq })
                }
                Ok((_, Ok(None))) => None,
                Ok((_, Err(error))) | Err(error) => {
                    store.record_error(&format!("bus delivery: {error}"));
                    None
                }
            }
        }
        BusFrameV1::Control { .. } => {
            // Kept for the admission check (`control.rs`).
            if let Err(error) = super::control::record(store, frame) {
                store.record_error(&format!("bus control: {error}"));
            }
            None
        }
        BusFrameV1::PresenceUpdate { .. } => {
            if let Err(error) = PresenceMap::mutate(data, |presence| presence.apply(frame)) {
                store.record_error(&format!("bus presence: {error}"));
            }
            None
        }
        BusFrameV1::HandoffOffer { .. } => {
            let registry = AgentRegistry::load_or_create();
            match Handoffs::mutate(data, |handoffs| {
                handoff::answer_offer(store, handoffs, &registry, frame)
            }) {
                Ok(reply) => reply,
                Err(error) => {
                    store.record_error(&format!("bus handoff: {error}"));
                    None
                }
            }
        }
        BusFrameV1::HandoffAck { .. } | BusFrameV1::HandoffReject { .. } => {
            if let Err(error) =
                Handoffs::mutate(data, |handoffs| handoff::answered(handoffs, frame))
            {
                store.record_error(&format!("bus handoff: {error}"));
            }
            None
        }
        BusFrameV1::Ping { nonce } => Some(BusFrameV1::Pong { nonce: *nonce }),
        BusFrameV1::Error {
            client_msg_id,
            error,
        } => {
            let refusal = crate::core::hive::HiveError::from_response(
                error.code.http_status(),
                &serde_json::json!({ "error": error }),
            );
            if matches!(
                refusal,
                crate::core::hive::HiveError::QuotaExceeded(_)
                    | crate::core::hive::HiveError::PolicyDenied(_)
            ) {
                // A used-up quota or a policy hold: the post stays local
                // (not marked forwarded) and goes again once the bus
                // throttle ends; nothing is published meanwhile.
                if let Some(id) = client_msg_id {
                    pending.remove(id);
                }
                store.record_error(&format!("bus: {refusal}"));
                let _ = crate::core::hive::throttle::note(
                    store,
                    crate::core::hive::throttle::Scope::Bus,
                    refusal,
                    chrono::Utc::now(),
                );
                return None;
            }
            if let Some(entry) = client_msg_id.as_ref().and_then(|id| pending.remove(id)) {
                // Refused for good: do not republish it on every tick.
                state.mark_forwarded(&entry);
                state.withheld += 1;
            } else if let Some(id) = client_msg_id {
                messages::refused(data, id, &error.message);
            }
            store.record_error(&format!("bus: {}", error.message));
            None
        }
        _ => None,
    }
}

/// Handoff offers that are due (their message is published) and expiries.
pub(super) fn due_offers(store: &HiveStore, state: &RelayState) -> Vec<BusFrameV1> {
    let data = store.data_dir();
    if Handoffs::load(data).offers.is_empty() {
        return Vec::new();
    }
    Handoffs::mutate(data, |handoffs| {
        handoff::due(handoffs, |entry| {
            state.forwarded.iter().any(|id| id == entry)
        })
    })
    .unwrap_or_else(|error| {
        store.record_error(&format!("bus handoff: {error}"));
        Vec::new()
    })
}
