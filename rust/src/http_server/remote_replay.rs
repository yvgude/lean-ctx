// SPDX-License-Identifier: Apache-2.0

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use std::io::{Read as _, Write as _};

const LEDGER_SCHEMA_VERSION: u32 = 2;
const MAX_LEDGER_BYTES: u64 = 1_000_000;
const MAX_LEDGER_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reservation {
    Reserved(u64),
    InFlight,
    Completed,
    Conflict,
}

#[derive(Debug)]
pub(super) struct RemoteReplayGuard {
    ledger_path: std::path::PathBuf,
    max_age_seconds: i64,
    relay: super::relay_replay_async::AsyncRelayReplayStore,
}

impl RemoteReplayGuard {
    pub(super) fn new(project_root: &str, max_age_seconds: i64) -> Self {
        Self {
            ledger_path: std::path::Path::new(project_root)
                .join(".lean-ctx")
                .join("a2a")
                .join("replay-v2.json"),
            max_age_seconds,
            relay: super::relay_replay_async::AsyncRelayReplayStore::new(
                project_root,
                max_age_seconds,
            ),
        }
    }

    #[cfg(test)]
    pub(super) async fn reserve(
        &self,
        authenticated_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Reservation, String> {
        self.reserve_with_alias(authenticated_id, None, now).await
    }

    pub(super) async fn reserve_with_alias(
        &self,
        authenticated_id: &str,
        legacy_alias: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Reservation, String> {
        self.reserve_inner(authenticated_id, legacy_alias, None, now)
            .await
    }

    async fn reserve_inner(
        &self,
        authenticated_id: &str,
        legacy_alias: Option<&str>,
        fingerprint: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Reservation, String> {
        let path = self.ledger_path.clone();
        let authenticated_id = authenticated_id.to_string();
        let legacy_alias = legacy_alias.map(str::to_string);
        let fingerprint = fingerprint.map(str::to_string);
        let max_age_seconds = self.max_age_seconds;
        tokio::task::spawn_blocking(move || {
            update_ledger(
                &path,
                &authenticated_id,
                Operation::Reserve {
                    now: now.timestamp(),
                    legacy_alias,
                    fingerprint,
                },
                max_age_seconds,
            )
        })
        .await
        .map_err(|error| format!("replay ledger task failed: {error}"))?
    }

    pub(super) async fn reserve_relay(
        &self,
        scoped_origin_id: &str,
        record: &crate::core::a2a::relay::RelayRecordV1,
        fingerprint: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<super::relay_replay::Reservation, super::relay_replay::StoreError> {
        self.relay
            .reserve(scoped_origin_id, record, fingerprint, now)
            .await
    }

    pub(super) async fn complete_relay(
        &self,
        scoped_origin_id: &str,
        lease: &str,
    ) -> Result<(), super::relay_replay::StoreError> {
        self.relay.complete(scoped_origin_id, lease).await
    }

    pub(super) async fn release_relay(
        &self,
        scoped_origin_id: &str,
        lease: &str,
    ) -> Result<(), super::relay_replay::StoreError> {
        self.relay.release(scoped_origin_id, lease).await
    }

    pub(super) async fn complete_relay_with_response(
        &self,
        scoped_origin_id: &str,
        lease: &str,
        body: String,
    ) -> Result<(), super::relay_replay::StoreError> {
        self.relay
            .complete_with_response(scoped_origin_id, lease, body)
            .await
    }

    pub(super) async fn completed_relay_response(
        &self,
        scoped_origin_id: &str,
        record: &crate::core::a2a::relay::RelayRecordV1,
        fingerprint: &str,
    ) -> Result<Option<String>, super::relay_replay::StoreError> {
        self.relay
            .completed_response(scoped_origin_id, record, fingerprint)
            .await
    }

    #[cfg(test)]
    pub(super) async fn complete(
        &self,
        authenticated_id: &str,
        generation: u64,
    ) -> Result<(), String> {
        self.complete_with_alias(authenticated_id, None, generation)
            .await
    }

    pub(super) async fn complete_with_alias(
        &self,
        authenticated_id: &str,
        legacy_alias: Option<&str>,
        generation: u64,
    ) -> Result<(), String> {
        self.transition(
            authenticated_id,
            Operation::Complete {
                generation,
                legacy_alias: legacy_alias.map(str::to_string),
            },
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn release(
        &self,
        authenticated_id: &str,
        generation: u64,
    ) -> Result<(), String> {
        self.release_with_alias(authenticated_id, None, generation)
            .await
    }

    pub(super) async fn release_with_alias(
        &self,
        authenticated_id: &str,
        legacy_alias: Option<&str>,
        generation: u64,
    ) -> Result<(), String> {
        self.transition(
            authenticated_id,
            Operation::Release {
                generation,
                legacy_alias: legacy_alias.map(str::to_string),
            },
        )
        .await
    }

    async fn transition(&self, authenticated_id: &str, operation: Operation) -> Result<(), String> {
        let path = self.ledger_path.clone();
        let authenticated_id = authenticated_id.to_string();
        let max_age_seconds = self.max_age_seconds;
        tokio::task::spawn_blocking(move || {
            update_ledger(&path, &authenticated_id, operation, max_age_seconds).map(|_| ())
        })
        .await
        .map_err(|error| format!("replay ledger task failed: {error}"))?
    }
}

#[derive(Debug, Clone)]
enum Operation {
    Reserve {
        now: i64,
        legacy_alias: Option<String>,
        fingerprint: Option<String>,
    },
    Complete {
        generation: u64,
        legacy_alias: Option<String>,
    },
    Release {
        generation: u64,
        legacy_alias: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayLedger {
    schema_version: u32,
    next_generation: u64,
    #[serde(default)]
    last_observed_at: i64,
    entries: std::collections::BTreeMap<String, ReplayEntry>,
}

impl Default for ReplayLedger {
    fn default() -> Self {
        Self {
            schema_version: LEDGER_SCHEMA_VERSION,
            next_generation: 1,
            last_observed_at: 0,
            entries: std::collections::BTreeMap::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplayEntry {
    accepted_at: i64,
    generation: u64,
    state: ReplayState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplayState {
    Pending,
    Completed,
}

fn update_ledger(
    path: &std::path::Path,
    authenticated_id: &str,
    operation: Operation,
    max_age_seconds: i64,
) -> Result<Reservation, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "replay ledger has no parent".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    reject_symlink(path)?;
    let lock_path = path.with_extension("lock");
    reject_symlink(&lock_path)?;
    let lock = open_lock(&lock_path)?;
    lock.lock_exclusive().map_err(|error| error.to_string())?;

    let result = (|| {
        let mut ledger = load_ledger(path)?;
        let reservation = match operation {
            Operation::Reserve {
                now,
                legacy_alias,
                fingerprint,
            } => reserve_entries(
                &mut ledger,
                authenticated_id,
                legacy_alias.as_deref(),
                now,
                max_age_seconds,
                fingerprint.as_deref(),
            )?,
            Operation::Complete {
                generation,
                legacy_alias,
            } => {
                transition_entries(
                    &mut ledger,
                    authenticated_id,
                    legacy_alias.as_deref(),
                    generation,
                    true,
                )?;
                Reservation::Completed
            }
            Operation::Release {
                generation,
                legacy_alias,
            } => {
                transition_entries(
                    &mut ledger,
                    authenticated_id,
                    legacy_alias.as_deref(),
                    generation,
                    false,
                )?;
                Reservation::Completed
            }
        };
        persist_ledger(path, parent, &ledger)?;
        Ok(reservation)
    })();
    let unlock = fs2::FileExt::unlock(&lock).map_err(|error| error.to_string());
    match (result, unlock) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(reservation), Ok(())) => Ok(reservation),
    }
}

fn load_ledger(path: &std::path::Path) -> Result<ReplayLedger, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplayLedger::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_LEDGER_BYTES {
        return Err("replay ledger exceeds size limit".to_string());
    }
    let mut bytes = Vec::new();
    file.take(MAX_LEDGER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES {
        return Err("replay ledger exceeds size limit".to_string());
    }
    let ledger: ReplayLedger = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid replay ledger: {error}"))?;
    if ledger.schema_version != LEDGER_SCHEMA_VERSION || ledger.entries.len() > MAX_LEDGER_ENTRIES {
        return Err("invalid replay ledger bounds or schema".to_string());
    }
    Ok(ledger)
}

fn reserve_entries(
    ledger: &mut ReplayLedger,
    authenticated_id: &str,
    legacy_alias: Option<&str>,
    now: i64,
    max_age_seconds: i64,
    fingerprint: Option<&str>,
) -> Result<Reservation, String> {
    let effective_now = now.max(ledger.last_observed_at);
    ledger.last_observed_at = effective_now;
    ledger.entries.retain(|_, entry| {
        effective_now
            .checked_sub(entry.accepted_at)
            .is_some_and(|age| (0..=max_age_seconds).contains(&age))
    });
    let mut ids = vec![authenticated_id];
    if let Some(alias) = legacy_alias
        && alias != authenticated_id
    {
        ids.push(alias);
    }
    if ids
        .iter()
        .filter_map(|id| ledger.entries.get(*id))
        .any(|entry| entry.fingerprint.as_deref() != fingerprint)
    {
        return Ok(Reservation::Conflict);
    }
    if ids
        .iter()
        .filter_map(|id| ledger.entries.get(*id))
        .any(|entry| entry.state == ReplayState::Completed)
    {
        return Ok(Reservation::Completed);
    }
    if ids
        .iter()
        .filter_map(|id| ledger.entries.get(*id))
        .any(|entry| entry.state == ReplayState::Pending)
    {
        return Ok(Reservation::InFlight);
    }
    let missing = ids
        .iter()
        .filter(|id| !ledger.entries.contains_key(**id))
        .count();
    if ledger.entries.len().saturating_add(missing) > MAX_LEDGER_ENTRIES {
        return Err("replay ledger entry limit reached".to_string());
    }
    let generation = ledger.next_generation;
    ledger.next_generation = generation
        .checked_add(1)
        .ok_or_else(|| "replay generation exhausted".to_string())?;
    for id in ids {
        ledger.entries.insert(
            id.to_string(),
            ReplayEntry {
                accepted_at: effective_now,
                generation,
                state: ReplayState::Pending,
                fingerprint: fingerprint.map(str::to_string),
            },
        );
    }
    Ok(Reservation::Reserved(generation))
}

fn transition_entries(
    ledger: &mut ReplayLedger,
    authenticated_id: &str,
    legacy_alias: Option<&str>,
    generation: u64,
    complete: bool,
) -> Result<(), String> {
    let mut ids = vec![authenticated_id];
    if let Some(alias) = legacy_alias
        && alias != authenticated_id
    {
        ids.push(alias);
    }
    for id in &ids {
        let entry = ledger
            .entries
            .get(*id)
            .ok_or_else(|| "replay reservation missing".to_string())?;
        if entry.generation != generation || entry.state != ReplayState::Pending {
            return Err("replay reservation ownership mismatch".to_string());
        }
    }
    if complete {
        for id in ids {
            ledger.entries.get_mut(id).expect("validated entry").state = ReplayState::Completed;
        }
    } else {
        for id in ids {
            ledger.entries.remove(id);
        }
    }
    Ok(())
}

#[cfg_attr(not(unix), allow(unused_variables))] // only the unix fsync reads `parent`
fn persist_ledger(
    path: &std::path::Path,
    parent: &std::path::Path,
    ledger: &ReplayLedger,
) -> Result<(), String> {
    let encoded = serde_json::to_vec(ledger).map_err(|error| error.to_string())?;
    if encoded.len() as u64 > MAX_LEDGER_BYTES {
        return Err("replay ledger exceeds size limit".to_string());
    }
    let temporary = path.with_extension("tmp");
    reject_symlink(&temporary)?;
    let mut file = open_temporary(&temporary)?;
    file.write_all(&encoded)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn reject_symlink(path: &std::path::Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("replay ledger path must not be a symlink".to_string())
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn open_lock(path: &std::path::Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    options.open(path).map_err(|error| error.to_string())
}

fn open_temporary(path: &std::path::Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    options.open(path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relay_binding_survives_reload_and_rejects_pending_and_completed_conflicts() {
        use super::super::relay_replay::Reservation as RelayReservation;
        let root = tempfile::tempdir().unwrap();
        let guard = RemoteReplayGuard::new(root.path().to_str().unwrap(), 86_730);
        let now = chrono::Utc::now();
        let record = super::super::relay_replay_async::test_record(now);
        let id = "a".repeat(64);
        let binding_a = "b".repeat(64);
        let binding_b = "c".repeat(64);
        let RelayReservation::Reserved(generation) = guard
            .reserve_relay(&id, &record, &binding_a, now)
            .await
            .unwrap()
        else {
            panic!("initial reservation");
        };
        assert_eq!(
            guard
                .reserve_relay(&id, &record, &binding_b, now)
                .await
                .unwrap(),
            RelayReservation::Conflict
        );
        assert_eq!(
            guard
                .reserve_relay(&id, &record, &binding_a, now)
                .await
                .unwrap(),
            RelayReservation::InFlight
        );
        guard.complete_relay(&id, &generation).await.unwrap();
        let reloaded = RemoteReplayGuard::new(root.path().to_str().unwrap(), 86_730);
        assert_eq!(
            reloaded
                .reserve_relay(&id, &record, &binding_a, now)
                .await
                .unwrap(),
            RelayReservation::Completed
        );
        assert_eq!(
            reloaded
                .reserve_relay(&id, &record, &binding_b, now)
                .await
                .unwrap(),
            RelayReservation::Conflict
        );
        // A relay binding neither occupies nor changes the legacy namespace.
        assert!(matches!(
            reloaded.reserve(&id, now).await.unwrap(),
            Reservation::Reserved(_)
        ));
    }
}
