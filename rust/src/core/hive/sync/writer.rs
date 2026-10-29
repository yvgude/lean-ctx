// SPDX-License-Identifier: Apache-2.0
//! What writes signed events into a workspace vault: the instance key, a
//! registered workspace worker and the place as C0 2.0 names it. Every
//! engine path that queues vault events (sync capture, `hive maintain`,
//! `knowledge contradict|retract`) builds its signer and place here, so the
//! C0 spellings (`hlc.node` and place id as hyphenated UUIDs) cannot drift
//! between paths again (dogfood 25.10.: the maintain CLI signed with the raw
//! `inst_<hex>` id and Hive refused `invalid_event shape /hlc/node`).

use crate::core::hive::auth::Session;
use crate::core::hive::events::build::{Place, Signer};
use crate::core::hive::events::sealed_body::SealKey;
use crate::core::hive::instance_key::InstanceKeys;
use crate::core::hive::replica::Replica;
use crate::core::hive::vault::VaultRecord;
use crate::core::hive::workers::card::{Availability, Budget, WorkerCard};
use crate::core::hive::workers::{attach, canonical_instance_id, register};
use crate::core::hive::{HiveError, HiveStore};

use super::place::{CAPTURE_HOST, place_id};

/// The instance key and a registered workspace worker of one role.
pub struct VaultWriter {
    pub keys: InstanceKeys,
    /// The canonical instance id (hyphenated UUID).
    pub instance_id: String,
    pub worker_id: String,
}

impl VaultWriter {
    /// Attach the `role` worker of this instance and make sure Hive knows
    /// it (E10), as the capture worker of a sync.
    pub fn open(session: &Session, role: &str) -> Result<Self, HiveError> {
        let keys = InstanceKeys::load(&session.store)?;
        let instance_id = canonical_instance_id(&session.instance.instance_id);
        let worker = attach::attach(&session.store, &instance_id, CAPTURE_HOST, role)?.worker;
        let worker = register::ensure_registered(
            session,
            &worker,
            &WorkerCard::new(
                CAPTURE_HOST,
                None,
                Vec::new(),
                Budget::default(),
                Availability::Online,
            ),
        )?;
        Ok(Self {
            keys,
            instance_id,
            worker_id: worker.worker_id,
        })
    }

    pub fn signer(&self) -> Signer<'_> {
        Signer::workspace(self.keys.signing_key(), &self.instance_id, &self.worker_id)
    }
}

/// `vault:<place id>`, the place reference of the E5/E6 routes.
pub fn vault_ref(record: &VaultRecord) -> String {
    format!("vault:{}", place_id(&record.vault_id))
}

/// The vault as an event place, keyed with the replica's id salt (the
/// sealed `pk_id` once a sealed vault synced) at its folded cursor.
pub fn vault_place(record: &VaultRecord, replica: &Replica) -> Result<Place, HiveError> {
    let pk_id = replica.id_salt()?.ok_or_else(|| {
        HiveError::Local("replica has no id_salt: run `lean-ctx hive sync`".into())
    })?;
    Ok(Place {
        kind: "vault".into(),
        id: place_id(&record.vault_id),
        pk_id,
        causal_base: replica.cursor()?.unwrap_or(0),
    })
}

/// A sealed vault's events are sealed with the newest epoch key held;
/// `None` for a plain vault.
pub fn vault_seal(store: &HiveStore, record: &VaultRecord) -> Result<Option<SealKey>, HiveError> {
    if !record.sealed {
        return Ok(None);
    }
    let missing = || HiveError::Refused("sealed_key_missing".into());
    let epoch = crate::core::hive::sealed::newest_held_epoch(store, &record.vault_id)?
        .ok_or_else(missing)?;
    let key =
        crate::core::hive::sealed::held_key(store, &record.vault_id, epoch).ok_or_else(missing)?;
    Ok(Some(SealKey { epoch, key }))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use crate::core::hive::events::build::Signer;

    /// The 1.x enrolment's `inst_<hex>` id signs as its UUID: the C0 2.0
    /// event schema only takes a UUID or 16 hex digits as `hlc.node`.
    #[test]
    fn a_workspace_signer_names_the_instance_as_its_uuid() {
        let key = SigningKey::from_bytes(&[0x49; 32]);
        for spelling in [
            "inst_6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
            "6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
            "6F2B9C4E-1A3D-4E5F-8A7B-0C1D2E3F4A5B",
        ] {
            let signer = Signer::workspace(&key, spelling, "wkr_vk4oelpv5lzjmev44hun");
            assert_eq!(
                signer.hlc_node, "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b",
                "{spelling}"
            );
        }
    }
}
