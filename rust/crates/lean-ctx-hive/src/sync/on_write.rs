// SPDX-License-Identifier: Apache-2.0
//! Push a knowledge write to Hive from the process that made it (#270).
//!
//! `ctx_knowledge` runs in the MCP server of the agent, the background
//! sync ([`super::auto`]) in the daemon: the daemon only saw a remembered
//! fact on its next poll, after its debounce and spacing. Here the writing
//! process syncs the vault of the checkout itself, about `COALESCE` after
//! the write and off the tool call, on its own thread — the `hive-calls`
//! thread of task claims never waits behind it.
//!
//! Bounded: requests of one checkout coalesce (a burst of writes is one
//! push), one push per `SPACING` per checkout in this process, and every
//! push takes its turn from the vault's budget that all processes of the
//! machine share (`super::budget`: a burst of `BURST_MAX` attempts, then
//! one per `BURST_EVERY`, none while a failure or Hive's `Retry-After` holds the
//! vault back). A vault another sync holds is tried again at most
//! `LOCK_RETRIES` times. Whatever does not go out here, the daemon's sync
//! takes along.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::state::HiveStore;
use crate::vault::VaultRecord;

/// Wait after the first request of a burst before the push.
pub(crate) const COALESCE: Duration = Duration::from_millis(300);
/// Least time between two pushes of one checkout.
pub(crate) const SPACING: Duration = Duration::from_secs(2);
/// How often a push the vault lock turned away is tried again.
pub(crate) const LOCK_RETRIES: u8 = 3;

/// What one push attempt came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Synced (or failed and noted in the status: the backoff holds it).
    Done,
    /// Another sync of the vault holds its lock: try again.
    Busy,
    /// Nothing to push from here (not connected, not shared, backing off).
    Skipped,
}

/// The checkouts waiting for a push.
#[derive(Debug, Default)]
pub(crate) struct Queue {
    /// Checkout → its earliest request not pushed yet.
    wants: BTreeMap<String, Instant>,
    /// Checkout → when its last push started.
    last: BTreeMap<String, Instant>,
    /// Checkout → lock retries so far.
    retries: BTreeMap<String, u8>,
}

impl Queue {
    /// Ask for a push of `root`; a request already waiting keeps its time.
    pub(crate) fn request(&mut self, root: &str, now: Instant) {
        self.wants.entry(root.to_string()).or_insert(now);
    }

    fn ready_at(&self, root: &str, requested: Instant) -> Instant {
        let coalesced = requested + COALESCE;
        match self.last.get(root) {
            Some(last) => coalesced.max(*last + SPACING),
            None => coalesced,
        }
    }

    /// The checkouts to push at `now`, and when the next one is due.
    pub(crate) fn due(&self, now: Instant) -> (Vec<String>, Option<Instant>) {
        let mut ready = Vec::new();
        let mut next: Option<Instant> = None;
        for (root, requested) in &self.wants {
            let at = self.ready_at(root, *requested);
            if at <= now {
                ready.push(root.clone());
            } else {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        (ready, next)
    }

    /// `root` is pushed from `now` on.
    pub(crate) fn take(&mut self, root: &str, now: Instant) {
        self.wants.remove(root);
        self.last
            .retain(|_, at| now.saturating_duration_since(*at) < SPACING);
        self.last.insert(root.to_string(), now);
    }

    /// Record how the push of `root` ended.
    pub(crate) fn finished(&mut self, root: &str, outcome: Outcome, now: Instant) {
        if outcome != Outcome::Busy {
            self.retries.remove(root);
            return;
        }
        let tries = self.retries.entry(root.to_string()).or_insert(0);
        *tries += 1;
        if *tries > LOCK_RETRIES {
            // The sync holding the lock or the daemon's next one takes it.
            self.retries.remove(root);
        } else {
            self.request(root, now);
        }
    }
}

/// Push every checkout due at `now` with `push`; the time the next one is
/// due. The queue is not locked while a push runs.
pub(crate) fn step(
    queue: &Mutex<Queue>,
    now: Instant,
    push: &mut impl FnMut(&str) -> Outcome,
) -> Option<Instant> {
    let ready = {
        let mut queue = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (ready, _) = queue.due(now);
        for root in &ready {
            queue.take(root, now);
        }
        ready
    };
    let ended: Vec<(String, Outcome)> = ready
        .into_iter()
        .map(|root| {
            let outcome = push(&root);
            (root, outcome)
        })
        .collect();
    let mut queue = queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (root, outcome) in ended {
        queue.finished(&root, outcome, now);
    }
    queue.due(now).1
}

fn state() -> &'static (Mutex<Queue>, Condvar) {
    static STATE: OnceLock<(Mutex<Queue>, Condvar)> = OnceLock::new();
    STATE.get_or_init(|| (Mutex::new(Queue::default()), Condvar::new()))
}

static STARTED: AtomicBool = AtomicBool::new(false);

/// Push the vault of the checkout `project_root` to Hive soon: call after
/// a knowledge write. Returns at once without touching the disk; whether
/// this machine is connected is looked at on the `hive-push` thread.
pub fn push_soon(project_root: &str) {
    #[cfg(any(test, feature = "test-support"))]
    record_request(project_root);
    let (queue, wake) = state();
    queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .request(project_root, Instant::now());
    wake.notify_all();
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("hive-push".into())
        .spawn(worker);
    if started.is_err() {
        // The daemon's sync takes the write along.
        STARTED.store(false, Ordering::SeqCst);
    }
}

/// Requests per checkout `push_soon` took (tests across crates observe
/// that a tool asks for a push).
#[cfg(any(test, feature = "test-support"))]
fn requests() -> &'static Mutex<BTreeMap<String, usize>> {
    static REQUESTS: OnceLock<Mutex<BTreeMap<String, usize>>> = OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}

#[cfg(any(test, feature = "test-support"))]
fn record_request(root: &str) {
    *requests()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(root.to_string())
        .or_default() += 1;
}

/// How many pushes of the checkout `root` were asked for in this process.
#[cfg(any(test, feature = "test-support"))]
pub fn requested(root: &str) -> usize {
    requests()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root)
        .copied()
        .unwrap_or(0)
}

/// Clears `STARTED` when the worker ends in any way (a panic included), so
/// the next write starts a new one instead of switching push on write off.
struct Running;

impl Drop for Running {
    fn drop(&mut self) {
        STARTED.store(false, Ordering::SeqCst);
    }
}

fn worker() {
    let _running = Running;
    let (queue, wake) = state();
    // A push that panics ends as done (the daemon takes it along); the
    // thread goes on with the next one.
    let mut push = |root: &str| {
        std::panic::catch_unwind(|| push_now(root)).unwrap_or_else(|_| {
            tracing::debug!("hive: push on write of {root} panicked");
            Outcome::Done
        })
    };
    loop {
        let now = Instant::now();
        let next = step(queue, now, &mut push);
        let guard = queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A request that came in during the push is due on the next step.
        let next = match guard.due(Instant::now()) {
            (ready, _) if !ready.is_empty() => continue,
            (_, later) => later.or(next),
        };
        match next {
            Some(at) => {
                let wait = at.saturating_duration_since(Instant::now());
                drop(wake.wait_timeout(guard, wait));
            }
            // Idle: sleep until the next request.
            None => drop(wake.wait(guard)),
        }
    }
}

/// The vault bound to the checkout `root`, as this machine recorded it.
fn bound_record(store: &HiveStore, root: &Path) -> Option<VaultRecord> {
    let stable =
        lean_ctx_protocol::hive::vault::stable_identity(&crate::vault::project_identity(root)?);
    crate::vault::VaultMap::load(store.data_dir())
        .vaults
        .get(&stable)
        .cloned()
}

/// Sync the vault of `root` now, as the daemon's pass does.
fn push_now(root: &str) -> Outcome {
    let Ok(store) = HiveStore::open() else {
        return Outcome::Skipped;
    };
    // Not connected to Hive: nothing to push.
    if !matches!(store.instance(), Ok(Some(_))) {
        return Outcome::Skipped;
    }
    let config = lean_ctx::core::config::Config::load().hive.clone();
    if config.enabled == Some(false) || !crate::bus::relay_v2::speaks_v2(&store) {
        return Outcome::Skipped;
    }
    let path = Path::new(root);
    let Some(record) = bound_record(&store, path) else {
        return Outcome::Skipped;
    };
    let mut screen = crate::knowledge::screening::Screen::for_installation();
    if screen.problem().is_some() {
        return Outcome::Skipped;
    }
    let lock = match super::auto::lock_vault(store.data_dir(), &record.vault_id, false) {
        Ok(Some(lock)) => lock,
        Ok(None) => return Outcome::Busy,
        Err(_) => return Outcome::Skipped,
    };
    // The vault's budget, shared with the daemon and every other process:
    // held back or used up, the daemon's sync takes the write along later.
    if super::budget::admit_sync(
        store.data_dir(),
        &record.vault_id,
        false,
        chrono::Utc::now(),
    ) != super::budget::Admit::Go
    {
        return Outcome::Skipped;
    }
    // Offline or lapsed: the daemon's sync backs off and retries.
    let Ok(session) = crate::scheduler::connected_session(&store, &config) else {
        return Outcome::Skipped;
    };
    let synced = super::place::sync_vault(&session, &record, path, &mut screen);
    if let Err(error) = &synced {
        tracing::debug!("hive: push on write of {} failed: {error}", record.vault_id);
    }
    let wait = super::auto::note_outside(
        &session.store,
        &record,
        super::auto::Trigger::Local,
        synced.as_ref(),
    );
    super::budget::settle(store.data_dir(), &record.vault_id, wait, chrono::Utc::now());
    drop(lock);
    Outcome::Done
}

#[cfg(test)]
#[path = "on_write_tests.rs"]
mod tests;
