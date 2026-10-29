// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx hive disconnect`: Hive is told first, local data goes after.
//!
//! The engine revokes its own instance (`DELETE /v1/instances/{id}`, C0
//! 2.0: the signed statement with reason `web_revoke`; on 1.x the seat is
//! freed). Only once Hive confirmed it, or answered that the instance is
//! already revoked or unknown, are the credentials and local data removed.
//! Unreachable, the disconnect stays pending (`disconnect-pending.json`):
//! nothing is removed and the scheduler retries it instead of syncing
//! (dogfood 25.10.: the CLI said "removed" while the instance stayed active
//! in Hive).

use serde::{Deserialize, Serialize};

use super::HiveError;
use super::auth::Session;
use super::config::HiveConfig;
use super::state::{HiveStore, InstanceRecord};

pub(crate) const PENDING_FILE: &str = "disconnect-pending.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    keep_local: bool,
}

/// What Hive said to the disconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Told {
    /// Hive revoked the instance now.
    Revoked,
    /// Hive already had it revoked or does not know it.
    AlreadyGone,
    /// Hive could not be reached; try again later.
    Unreachable(HiveError),
    /// Hive refused (or no session could be built); nothing was removed.
    Refused(HiveError),
}

impl Told {
    /// Whether the local data may go now.
    pub fn done(&self) -> bool {
        matches!(self, Self::Revoked | Self::AlreadyGone)
    }
}

/// Classify the answer to the revoke call.
pub fn classify(answer: Result<(), HiveError>) -> Told {
    match answer {
        Ok(()) => Told::Revoked,
        Err(HiveError::Revoked(_) | HiveError::NotFound) => Told::AlreadyGone,
        Err(e) if e.is_transient() => Told::Unreachable(e),
        Err(e) => Told::Refused(e),
    }
}

/// Ask Hive to revoke `instance`.
pub fn tell_hive(store: &HiveStore, config: &HiveConfig, instance: &InstanceRecord) -> Told {
    // A revocation recorded before means Hive already ended it.
    if super::revoked::recorded(store, instance).is_some() {
        return Told::AlreadyGone;
    }
    classify(
        super::root_cert::trust_root(store, config, &instance.url)
            .and_then(|root| Session::load(store.clone(), root))
            .and_then(|session| session.delete_instance()),
    )
}

/// Remember an unfinished disconnect (with its `--keep-local`).
pub fn mark_pending(store: &HiveStore, keep_local: bool) -> Result<(), HiveError> {
    store.write_config(PENDING_FILE, &Pending { keep_local })
}

/// The pending disconnect's `keep_local`, if one is pending.
pub fn pending(store: &HiveStore) -> Option<bool> {
    store
        .read_config::<Pending>(PENDING_FILE)
        .ok()
        .flatten()
        .map(|p| p.keep_local)
}

/// The scheduler's retry of a pending disconnect: `Some(line)` when one was
/// pending (finished, or why not yet).
pub fn retry_pending(
    store: &HiveStore,
    config: &HiveConfig,
    instance: &InstanceRecord,
) -> Option<Result<String, HiveError>> {
    let keep_local = pending(store)?;
    Some(match tell_hive(store, config, instance) {
        Told::Revoked | Told::AlreadyGone => store.forget(keep_local).map(|()| {
            format!(
                "disconnected {} (pending disconnect finished)",
                instance.label
            )
        }),
        Told::Unreachable(e) | Told::Refused(e) => Err(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_data_goes_only_after_hive_answered() {
        assert!(classify(Ok(())).done());
        assert!(classify(Err(HiveError::NotFound)).done());
        assert!(
            classify(Err(HiveError::Revoked(
                crate::core::hive::Revocation::default()
            )))
            .done()
        );
        assert!(matches!(
            classify(Err(HiveError::Offline("dns".into()))),
            Told::Unreachable(_)
        ));
        assert!(matches!(
            classify(Err(HiveError::Forbidden)),
            Told::Refused(_)
        ));
    }

    #[test]
    fn an_unreachable_disconnect_stays_pending_and_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        assert_eq!(pending(&store), None);
        mark_pending(&store, true).unwrap();
        assert_eq!(pending(&store), Some(true));
        // Disconnecting for good removes the marker with the credentials.
        store.forget(false).unwrap();
        assert_eq!(pending(&store), None);
    }
}
