// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud disconnect`: Cloud is told first, local data goes after.
//!
//! The engine revokes its own instance (`DELETE /v1/instances/{id}`, C0
//! 2.0: the signed statement with reason `web_revoke`; on 1.x the seat is
//! freed). Only once Cloud confirmed it, or answered that the instance is
//! already revoked or unknown, are the credentials and local data removed.
//! Unreachable, the disconnect stays pending (`disconnect-pending.json`):
//! nothing is removed and the scheduler retries it instead of syncing
//! (dogfood 25.10.: the CLI said "removed" while the instance stayed active
//! in Cloud).

use serde::{Deserialize, Serialize};

use super::CloudError;
use super::auth::Session;
use super::config::CloudConfig;
use super::state::{CloudStore, InstanceRecord};

pub(crate) const PENDING_FILE: &str = "disconnect-pending.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    keep_local: bool,
}

/// What Cloud said to the disconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Told {
    /// Cloud revoked the instance now.
    Revoked,
    /// Cloud already had it revoked or does not know it.
    AlreadyGone,
    /// Cloud could not be reached; try again later.
    Unreachable(CloudError),
    /// Cloud refused (or no session could be built); nothing was removed.
    Refused(CloudError),
}

impl Told {
    /// Whether the local data may go now.
    pub fn done(&self) -> bool {
        matches!(self, Self::Revoked | Self::AlreadyGone)
    }
}

/// Classify the answer to the revoke call.
pub fn classify(answer: Result<(), CloudError>) -> Told {
    match answer {
        Ok(()) => Told::Revoked,
        Err(CloudError::Revoked(_) | CloudError::NotFound) => Told::AlreadyGone,
        Err(e) if e.is_transient() => Told::Unreachable(e),
        Err(e) => Told::Refused(e),
    }
}

/// Ask Cloud to revoke `instance`.
pub fn tell_cloud(store: &CloudStore, config: &CloudConfig, instance: &InstanceRecord) -> Told {
    // A revocation recorded before means Cloud already ended it.
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
pub fn mark_pending(store: &CloudStore, keep_local: bool) -> Result<(), CloudError> {
    store.write_config(PENDING_FILE, &Pending { keep_local })
}

/// The pending disconnect's `keep_local`, if one is pending.
pub fn pending(store: &CloudStore) -> Option<bool> {
    store
        .read_config::<Pending>(PENDING_FILE)
        .ok()
        .flatten()
        .map(|p| p.keep_local)
}

/// The scheduler's retry of a pending disconnect: `Some(line)` when one was
/// pending (finished, or why not yet).
pub fn retry_pending(
    store: &CloudStore,
    config: &CloudConfig,
    instance: &InstanceRecord,
) -> Option<Result<String, CloudError>> {
    let keep_local = pending(store)?;
    Some(match tell_cloud(store, config, instance) {
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
    fn local_data_goes_only_after_cloud_answered() {
        assert!(classify(Ok(())).done());
        assert!(classify(Err(CloudError::NotFound)).done());
        assert!(classify(Err(CloudError::Revoked(crate::Revocation::default()))).done());
        assert!(matches!(
            classify(Err(CloudError::Offline("dns".into()))),
            Told::Unreachable(_)
        ));
        assert!(matches!(
            classify(Err(CloudError::Forbidden)),
            Told::Refused(_)
        ));
    }

    #[test]
    fn an_unreachable_disconnect_stays_pending_and_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let store = CloudStore::at(dir.path().join("c"), dir.path().join("d"));
        assert_eq!(pending(&store), None);
        mark_pending(&store, true).unwrap();
        assert_eq!(pending(&store), Some(true));
        // Disconnecting for good removes the marker with the credentials.
        store.forget(false).unwrap();
        assert_eq!(pending(&store), None);
    }
}
