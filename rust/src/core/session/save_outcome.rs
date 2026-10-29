// SPDX-License-Identifier: Apache-2.0
//! Commit acknowledgement without replacing edits made during background I/O.

use super::{SaveError, SessionState};
use crate::core::context_checkpoint::CanonicalSession;
use crate::server::bounded_lock::{read_for, write_for};
use lean_ctx_protocol::Sha256Digest;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const SYNC_SAVE_GUARD_WAIT: Duration = Duration::from_millis(250);
const SYNC_SAVE_LANE_WAIT: Duration = Duration::from_secs(5);
/// How long the shutdown save WAITS FOR THE LANE — not a limit on the save.
/// A background save in flight when the connection closes holds the lane for
/// one projection of the session. Once the shutdown save has the lane it
/// runs to its end: it is the thing to finish, and a timeout could not stop
/// its blocking work anyway.
const SHUTDOWN_SAVE_LANE_WAIT: Duration = Duration::from_secs(15);
/// How long the ONE background save that follows a save in flight waits for
/// the lane. Bounded like every wait here; there is at most one such waiter
/// per session, so the bound can cover a slow save instead of giving up
/// behind it call after call.
const FOLLOWING_SAVE_LANE_WAIT: Duration = Duration::from_mins(1);

/// The save lane of one live session, shared by its clones.
///
/// `lane` is held from the snapshot through the acknowledgement of a save.
/// Background saves are requested after tool calls and at checkpoints; while
/// one is in flight, exactly ONE further request waits behind it (`follower`)
/// and every other request returns at once — the follower takes its snapshot
/// when it gets the lane, so it saves whatever is current then, and a third
/// request has nothing to add. `closing` is set by the shutdown save: a
/// follower that gets the lane afterwards yields it unused, because the
/// shutdown save is about to store the last state itself.
#[derive(Debug, Default)]
pub(crate) struct SaveGate {
    lane: tokio::sync::Mutex<()>,
    follower: AtomicBool,
    closing: AtomicBool,
}

pub(super) struct SaveOutcome {
    id: String,
    version: u32,
    expected: Option<Sha256Digest>,
    pub(super) primary_sha256: Option<String>,
    pub(super) committed: Option<Box<CanonicalSession>>,
    /// `Err` leaves the pending changes pending: admission refused the
    /// session, the admitting authority changed before the commit, or the
    /// commit itself failed.
    pub(super) result: Result<(), SaveError>,
}

pub(crate) struct SharedSaveReceipt {
    pub(crate) id: String,
    pub(crate) version: u32,
    /// True only when the owner still has the exact version saved by this call.
    pub(crate) current: bool,
}

impl SaveOutcome {
    pub(super) fn new(id: &str, version: u32, expected: Option<Sha256Digest>) -> Self {
        Self {
            id: id.to_owned(),
            version,
            expected,
            primary_sha256: None,
            committed: None,
            result: Ok(()),
        }
    }
}

impl SessionState {
    pub(super) fn note_save_failure(&mut self) {
        self.last_save_failed = true;
        self.stats.unsaved_changes = self
            .stats
            .unsaved_changes
            .max(super::state::BATCH_SAVE_INTERVAL);
    }

    pub(super) fn acknowledge_save(&mut self, outcome: SaveOutcome) -> Result<(), String> {
        if self.id != outcome.id {
            return outcome.result.map_err(String::from);
        }
        let same_base = self
            .canonical_checkpoint
            .as_ref()
            .and_then(|binding| binding.storage_digest.as_ref())
            == outcome.expected.as_ref();
        if same_base && let Some(committed) = outcome.committed {
            // Includes a primary commit followed by a pointer-write failure.
            // Update only the binding; pending task/findings/etc remain untouched.
            self.canonical_checkpoint = Some(committed);
        }
        if outcome.result.is_err() {
            self.note_save_failure();
        } else if same_base && self.version == outcome.version {
            self.stats.unsaved_changes = 0;
            self.last_save_failed = false;
            self.last_flush = Some(std::time::Instant::now());
        }
        outcome.result.map_err(String::from)
    }

    /// Flush the shared live owner without holding its state lock during disk I/O.
    /// The per-owner lane covers preparation through acknowledgement, not state.
    pub async fn save_shared(owner: Arc<RwLock<Self>>) -> Result<(), String> {
        Self::save_shared_within(owner, SYNC_SAVE_LANE_WAIT).await
    }

    /// [`Self::save_shared`] with the wait for the save lane given by the
    /// caller. The wait stays bounded; only the shutdown save takes a longer
    /// one, because it is the last chance to make the current state durable.
    async fn save_shared_within(
        owner: Arc<RwLock<Self>>,
        lane_wait: Duration,
    ) -> Result<(), String> {
        let gate = owner.read().await.save_gate.clone();
        let lane = tokio::time::timeout(lane_wait, gate.lane.lock()).await;
        let Ok(_lane) = lane else {
            let mut live = owner.write().await;
            if Arc::ptr_eq(&gate, &live.save_gate) {
                live.note_save_failure();
            }
            return Err("session save lane unavailable".into());
        };
        Self::save_in_lane(owner, &gate).await
    }

    /// A save requested in the background (after a tool call, at a
    /// checkpoint, at idle). See [`SaveGate`]: one save in flight, one
    /// behind it, the rest return — their state is covered by the follower's
    /// later snapshot. A follower whose state was already acknowledged by
    /// the save it waited behind does nothing.
    async fn save_shared_background(owner: Arc<RwLock<Self>>) -> Result<(), String> {
        let gate = owner.read().await.save_gate.clone();
        if gate.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        let _lane = if let Ok(lane) = gate.lane.try_lock() {
            lane
        } else {
            if gate.follower.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            let waited = tokio::time::timeout(FOLLOWING_SAVE_LANE_WAIT, gate.lane.lock()).await;
            // Cleared BEFORE the snapshot below: a request that arrives from
            // here on is not covered by it and becomes the next follower.
            gate.follower.store(false, Ordering::Release);
            let Ok(lane) = waited else {
                let mut live = owner.write().await;
                if Arc::ptr_eq(&gate, &live.save_gate) {
                    live.note_save_failure();
                }
                return Err("session save lane unavailable".into());
            };
            lane
        };
        if gate.closing.load(Ordering::Acquire) {
            return Ok(());
        }
        {
            let live = owner.read().await;
            if Arc::ptr_eq(&gate, &live.save_gate)
                && live.stats.unsaved_changes == 0
                && !live.last_save_failed
            {
                return Ok(());
            }
        }
        Self::save_in_lane(owner, &gate).await
    }

    /// The save itself. The caller holds `gate`'s lane for the whole call.
    async fn save_in_lane(owner: Arc<RwLock<Self>>, gate: &Arc<SaveGate>) -> Result<(), String> {
        let (id, version, mut snapshot) = {
            let session = owner.read().await;
            if !Arc::ptr_eq(&gate, &session.save_gate) {
                return Ok(()); // The scheduled owner was replaced, not the new session.
            }
            (session.id.clone(), session.version, session.clone())
        };
        // `prepare_save` only detaches the copy (and keeps this task's request
        // project with it). Storage admission, serialization, the authority
        // re-check and the disk I/O all run on the blocking pool, outside the
        // live session lock and off the async executor. The save lane still
        // spans prepare, admission, disk I/O and ack.
        let prepared = match snapshot.prepare_save() {
            Ok(prepared) => prepared,
            Err(error) => {
                let mut live = owner.write().await;
                if Arc::ptr_eq(&gate, &live.save_gate) && live.id == id && live.version == version {
                    live.note_save_failure();
                }
                return Err(error);
            }
        };
        if let Ok(outcome) =
            tokio::task::spawn_blocking(move || prepared.write_to_disk_with_policy(false)).await
        {
            let mut live = owner.write().await;
            if !Arc::ptr_eq(&gate, &live.save_gate) {
                return Ok(());
            }
            live.acknowledge_save(outcome)
        } else {
            let mut live = owner.write().await;
            if Arc::ptr_eq(&gate, &live.save_gate) && live.id == id && live.version == version {
                live.note_save_failure();
            }
            Err("session save worker unavailable; verify committed state before retrying".into())
        }
    }

    /// Synchronous MCP adapter with the same save lane, detached projection,
    /// version check, and owner acknowledgement as save_shared.
    pub(crate) fn save_shared_sync(owner: &Arc<RwLock<Self>>) -> Result<SharedSaveReceipt, String> {
        let gate = read_for(owner, "session save owner", SYNC_SAVE_GUARD_WAIT)
            .ok_or_else(|| "session is busy; retry the save".to_string())?
            .save_gate
            .clone();
        let lane_deadline = Instant::now() + SYNC_SAVE_LANE_WAIT;
        let _lane = loop {
            match gate.lane.try_lock() {
                Ok(lane) => break lane,
                Err(_) if Instant::now() < lane_deadline => {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => {
                    if let Some(mut live) =
                        write_for(owner, "session save owner", SYNC_SAVE_GUARD_WAIT)
                        && Arc::ptr_eq(&gate, &live.save_gate)
                    {
                        live.note_save_failure();
                    }
                    return Err("session save lane unavailable".into());
                }
            }
        };
        let (id, version, mut snapshot) = {
            let session = read_for(owner, "session save owner", SYNC_SAVE_GUARD_WAIT)
                .ok_or_else(|| "session is busy; retry the save".to_string())?;
            if !Arc::ptr_eq(&gate, &session.save_gate) {
                return Err("session owner changed before save; retry".into());
            }
            (session.id.clone(), session.version, session.clone())
        };
        let prepared = match snapshot.prepare_save() {
            Ok(prepared) => prepared,
            Err(error) => {
                if let Some(mut live) = write_for(owner, "session save owner", SYNC_SAVE_GUARD_WAIT)
                    && Arc::ptr_eq(&gate, &live.save_gate)
                    && live.id == id
                    && live.version == version
                {
                    live.note_save_failure();
                }
                return Err(error);
            }
        };
        let outcome = prepared.write_to_disk_with_policy(false);
        let Some(mut live) = write_for(owner, "session save owner", SYNC_SAVE_GUARD_WAIT) else {
            return Err(
                "session save outcome could not be acknowledged; verify durable state before retrying"
                    .into(),
            );
        };
        if !Arc::ptr_eq(&gate, &live.save_gate) || live.id != id {
            return Err(
                "session owner changed after save; current state was not acknowledged".into(),
            );
        }
        let same_base = live
            .canonical_checkpoint
            .as_ref()
            .and_then(|binding| binding.storage_digest.as_ref())
            == outcome.expected.as_ref();
        live.acknowledge_save(outcome)?;
        Ok(SharedSaveReceipt {
            id,
            version,
            current: same_base
                && live.version == version
                && live.stats.unsaved_changes == 0
                && !live.last_save_failed
                && Arc::ptr_eq(&gate, &live.save_gate),
        })
    }

    pub(crate) async fn save_shared_logged(owner: Arc<RwLock<Self>>) {
        if let Err(error) = Self::save_shared_background(owner).await {
            tracing::warn!(%error, "session persistence failed; unsaved state retained");
        }
    }

    /// The save of a server that is ending: the same save, behind the same
    /// lane. Background followers are told to yield ([`SaveGate`]); a save
    /// already in flight is waited for up to [`SHUTDOWN_SAVE_LANE_WAIT`].
    ///
    /// `true`: the owner's current version is acknowledged as durable.
    /// `false`: it is not, and the log says why. A save that failed (lane not
    /// free in time, admission refused, authority changed, commit failed) is
    /// not repeated blindly — its changes stay pending as after any failed
    /// save, and there is no later save in this process to pick them up. A
    /// session that CHANGED while it was being saved is saved once more.
    pub(crate) async fn save_shared_at_shutdown(owner: Arc<RwLock<Self>>) -> bool {
        let gate = owner.read().await.save_gate.clone();
        gate.closing.store(true, Ordering::Release);
        let durable = Self::save_last_state(&owner, SHUTDOWN_SAVE_LANE_WAIT).await;
        // A server object can outlive this call (a connection of a shared
        // server dropped): background saves work again afterwards.
        gate.closing.store(false, Ordering::Release);
        durable
    }

    async fn save_last_state(owner: &Arc<RwLock<Self>>, lane_wait: Duration) -> bool {
        for _attempt in 0..2 {
            let saved = Self::save_shared_within(Arc::clone(owner), lane_wait).await;
            if let Err(error) = &saved {
                tracing::warn!(%error, "session persistence failed at shutdown; the last state is not durable");
                return false;
            }
            let live = owner.read().await;
            if live.stats.unsaved_changes == 0 && !live.last_save_failed {
                return true;
            }
        }
        tracing::warn!(
            "session kept changing during the shutdown saves; the last state is not durable"
        );
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionState, SharedSaveReceipt};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn synchronous_shared_save_needs_no_tokio_runtime() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let mut session = SessionState::new();
        session.id = "sync-shared-save".into();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.stats.unsaved_changes = 1;

        let owner = Arc::new(RwLock::new(session));
        let receipt: SharedSaveReceipt =
            SessionState::save_shared_sync(&owner).expect("synchronous session save");

        assert!(receipt.current, "saved version should be acknowledged");
        let saved = crate::core::session::paths::sessions_dir()
            .expect("session directory")
            .join(format!("{}.json", receipt.id));
        assert!(saved.is_file(), "the save adapter must write the session");
        let live = owner.try_read().expect("uncontested session");
        assert_eq!(live.stats.unsaved_changes, 0);
        assert!(!live.last_save_failed);
    }

    #[test]
    fn newer_live_version_stays_dirty_after_older_save_ack() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let mut snapshot = SessionState::new();
        snapshot.id = "sync-save-newer-version".into();
        snapshot.project_root = Some(project.path().to_string_lossy().into_owned());
        snapshot.version = 1;
        snapshot.stats.unsaved_changes = 1;

        let prepared = snapshot.prepare_save().expect("prepare snapshot");
        let outcome = prepared.write_to_disk_with_policy(false);
        assert!(outcome.result.is_ok(), "fixture save should succeed");

        let mut live = snapshot;
        live.version = 2;
        live.stats.unsaved_changes = 2;
        live.acknowledge_save(outcome)
            .expect("acknowledge older save");

        assert_eq!(live.version, 2);
        assert_eq!(live.stats.unsaved_changes, 2);
    }

    /// A live session with one unsaved change in its own data directory,
    /// under no project policy (environment first, policy second), and the
    /// file its save writes. The guards stay on the test thread, outside the
    /// async block the test drives.
    struct HeldStill {
        _data: crate::core::data_dir::IsolatedDataDir,
        _community: crate::core::policy::runtime::TestPolicyOverride,
        _project: tempfile::TempDir,
    }

    fn unsaved_owner(id: &str) -> (HeldStill, Arc<RwLock<SessionState>>, std::path::PathBuf) {
        let data = crate::core::data_dir::isolated_data_dir();
        let community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project tempdir");
        let mut session = SessionState::new();
        session.id = id.into();
        session.project_root = Some(project.path().to_string_lossy().into_owned());
        session.increment();
        let saved = crate::core::session::paths::sessions_dir()
            .expect("session directory")
            .join(format!("{id}.json"));
        let held = HeldStill {
            _data: data,
            _community: community,
            _project: project,
        };
        (held, Arc::new(RwLock::new(session)), saved)
    }

    fn drive<T>(work: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(work)
    }

    async fn until(condition: impl Fn() -> bool) {
        for _ in 0..2000 {
            if condition() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("condition not reached");
    }

    /// While a save holds the lane, ONE background request waits behind it
    /// and every further one returns at once without saving and without
    /// marking a failure; the follower then saves the state that is current
    /// when it gets the lane. Before, each request waited five seconds,
    /// failed and marked the session — one per tool call.
    #[test]
    fn while_a_save_is_in_flight_one_request_follows_and_the_rest_return() {
        let (_held_still, owner, saved) = unsaved_owner("save-lane-follower");
        drive(async {
            let gate = owner.read().await.save_gate.clone();
            let in_flight = gate.lane.lock().await;
            let follower = tokio::spawn(SessionState::save_shared_background(Arc::clone(&owner)));
            until(|| gate.follower.load(super::Ordering::Acquire)).await;

            for _ in 0..3 {
                SessionState::save_shared_background(Arc::clone(&owner))
                    .await
                    .expect("a covered request is no failure");
            }
            assert!(!follower.is_finished(), "the follower waits for the lane");
            assert!(!saved.exists(), "nobody saved behind the save in flight");
            assert!(!owner.read().await.last_save_failed);

            // A change made while the follower waits is part of what it saves.
            owner.write().await.increment();
            drop(in_flight);
            follower.await.expect("follower").expect("follower saves");

            assert!(saved.is_file());
            let live = owner.read().await;
            assert_eq!(live.stats.unsaved_changes, 0, "both changes acknowledged");
            assert!(!gate.follower.load(super::Ordering::Acquire));
        });
    }

    /// The shutdown save: a background follower yields the lane unused, the
    /// last state is stored and reported durable, and background saves work
    /// again afterwards. A lane that does not come free within the wait is
    /// reported as NOT durable — never as saved.
    #[test]
    fn the_shutdown_save_reports_whether_the_last_state_is_durable() {
        let (_held_still, owner, saved) = unsaved_owner("save-lane-shutdown");
        drive(async {
            let gate = owner.read().await.save_gate.clone();
            let in_flight = gate.lane.lock().await;

            let short = std::time::Duration::from_millis(50);
            assert!(
                !SessionState::save_last_state(&owner, short).await,
                "a lane that stays taken is not a durable last state"
            );
            assert!(owner.read().await.last_save_failed);
            assert!(!saved.exists());

            let follower = tokio::spawn(SessionState::save_shared_background(Arc::clone(&owner)));
            until(|| gate.follower.load(super::Ordering::Acquire)).await;
            let shutdown = tokio::spawn(SessionState::save_shared_at_shutdown(Arc::clone(&owner)));
            until(|| gate.closing.load(super::Ordering::Acquire)).await;
            drop(in_flight);

            follower
                .await
                .expect("follower")
                .expect("yielding is no failure");
            assert!(
                shutdown.await.expect("shutdown save"),
                "the last state is durable"
            );
            assert!(saved.is_file());
            let live = owner.read().await;
            assert_eq!(live.stats.unsaved_changes, 0);
            assert!(!live.last_save_failed);
            assert!(
                !gate.closing.load(super::Ordering::Acquire),
                "background saves resume"
            );
        });
    }
}
