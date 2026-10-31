// SPDX-License-Identifier: Apache-2.0
//! Display snapshots for slow dashboard reads.
//!
//! Some reads re-verify ledgers or probe health and take seconds. The
//! Cockpit polls them, so they are answered from a short-lived snapshot:
//!
//! * within the route's TTL the last snapshot is served as is;
//! * within its `STALE_OK` window the last snapshot is served at once and a
//!   fresh one is computed in the background — one computation per route
//!   and project at a time (single flight);
//! * otherwise the read is computed now; concurrent reads wait for it.
//!
//! The slowest reads (`/api/roi`, `/api/value`, `/api/health`) are also kept
//! on disk under `<data>/dashboard-snapshots/<partition>/` (atomic, 0600)
//! and refreshed when the server starts, so the first visit is instant.
//!
//! Snapshots are display data only: every response says when it was
//! observed (`X-LeanCTX-Observed-At`), whether it is a snapshot
//! (`X-LeanCTX-Snapshot: cached|fresh`) and whether a refresh is running
//! (`X-LeanCTX-Refreshing: 0|1`). Nothing decides on them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// `(status, content type, body)` as returned by the route handlers.
pub(crate) type RouteResponse = (&'static str, &'static str, String);
/// Computes one read; runs on the request thread or a background thread.
pub(crate) type Compute = Arc<dyn Fn() -> RouteResponse + Send + Sync>;

const SNAPSHOT_DIR: &str = "dashboard-snapshots";
const SNAPSHOT_FILE: &str = "displays.json";
/// How long a read waits for a refresh another read already started.
const WAIT_FOR_REFRESH: Duration = Duration::from_secs(125);
const SNAPSHOT_SCHEMA: u32 = 1;

/// The display TTL of a route, with the route as a `'static` key.
fn display_ttl(route: &str) -> Option<(&'static str, Duration)> {
    let (route, secs): (&'static str, u64) = match route {
        "/api/stats" => ("/api/stats", 3),
        "/api/roi" => ("/api/roi", 30),
        "/api/value" => ("/api/value", 15),
        "/api/gain" => ("/api/gain", 15),
        "/api/health" => ("/api/health", 10),
        "/api/version" => ("/api/version", 600),
        _ => return None,
    };
    Some((route, Duration::from_secs(secs)))
}

/// How old a snapshot may be and still be served while a fresh one is
/// computed. These routes are also the ones kept on disk.
fn stale_ok(route: &str) -> Option<Duration> {
    let secs = match route {
        "/api/roi" => 300,
        "/api/value" => 900,
        "/api/health" => 120,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
}

const PERSISTED: &[&str] = &["/api/roi", "/api/value", "/api/health"];

/// Whether `route` is answered through display snapshots at all.
pub(crate) fn is_display_route(route: &str) -> bool {
    display_ttl(route).is_some()
}

#[derive(Clone)]
struct Snapshot {
    computed: Instant,
    observed_at: SystemTime,
    content_type: &'static str,
    body: String,
}

impl Snapshot {
    fn response(&self) -> RouteResponse {
        ("200 OK", self.content_type, self.body.clone())
    }
}

/// How a response relates to the snapshot store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    pub(crate) observed_at: SystemTime,
    pub(crate) cached: bool,
    pub(crate) refreshing: bool,
}

impl Observation {
    /// The response headers that label display data.
    pub(crate) fn headers(&self) -> String {
        let observed: chrono::DateTime<chrono::Utc> = self.observed_at.into();
        format!(
            "X-LeanCTX-Observed-At: {}\r\nX-LeanCTX-Snapshot: {}\r\nX-LeanCTX-Refreshing: {}\r\n",
            observed.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            if self.cached { "cached" } else { "fresh" },
            if self.refreshing { "1" } else { "0" },
        )
    }
}

type Key = (String, &'static str);

#[derive(Default)]
struct State {
    cache: HashMap<Key, Snapshot>,
    pending: HashSet<Key>,
    loaded: HashSet<String>,
}

/// The snapshot store of one dashboard server.
pub(crate) struct DisplaySnapshots {
    state: Mutex<State>,
    settled: Condvar,
    /// `<data>/dashboard-snapshots`; `None` keeps snapshots in memory only.
    dir: Option<PathBuf>,
}

/// Clears a pending refresh even when the computation panics.
struct PendingGuard<'a> {
    store: &'a DisplaySnapshots,
    key: Key,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.store.lock();
        state.pending.remove(&self.key);
        drop(state);
        self.store.settled.notify_all();
    }
}

fn valid_partition(partition: &str) -> bool {
    !partition.is_empty()
        && partition.len() <= 64
        && partition
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl DisplaySnapshots {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self {
            state: Mutex::new(State::default()),
            settled: Condvar::new(),
            dir,
        }
    }

    /// The store of this process, persisted under the LeanCTX data directory.
    pub(crate) fn global() -> Arc<Self> {
        static GLOBAL: std::sync::LazyLock<Arc<DisplaySnapshots>> =
            std::sync::LazyLock::new(|| {
                let dir = crate::core::data_dir::lean_ctx_data_dir()
                    .ok()
                    .map(|d| d.join(SNAPSHOT_DIR));
                Arc::new(DisplaySnapshots::new(dir))
            });
        GLOBAL.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn file(&self, partition: &str) -> Option<PathBuf> {
        valid_partition(partition)
            .then(|| {
                self.dir
                    .as_ref()
                    .map(|d| d.join(partition).join(SNAPSHOT_FILE))
            })
            .flatten()
    }

    /// Answer `route` for `partition`, from a snapshot where its TTL allows.
    /// Routes without a display TTL are computed directly, unlabelled.
    pub(crate) fn serve(
        self: &Arc<Self>,
        route: &str,
        partition: &str,
        compute: &Compute,
    ) -> (RouteResponse, Option<Observation>) {
        let Some((route, ttl)) = display_ttl(route) else {
            return (compute(), None);
        };
        self.load_partition(partition);
        let key: Key = (partition.to_string(), route);
        let requested_at = Instant::now();
        let mut state = self.lock();
        if let Some(snapshot) = state.cache.get(&key) {
            let age = snapshot.computed.elapsed();
            if age < ttl {
                let observation = Observation {
                    observed_at: snapshot.observed_at,
                    cached: true,
                    refreshing: false,
                };
                return (snapshot.response(), Some(observation));
            }
            if stale_ok(route).is_some_and(|window| age <= window) {
                let snapshot = snapshot.clone();
                if state.pending.insert(key.clone()) {
                    drop(state);
                    self.refresh_in_background(key, compute.clone());
                }
                let observation = Observation {
                    observed_at: snapshot.observed_at,
                    cached: true,
                    refreshing: true,
                };
                return (snapshot.response(), Some(observation));
            }
        }
        if state.pending.contains(&key) {
            let (waited, timeout) = self
                .settled
                .wait_timeout_while(state, WAIT_FOR_REFRESH, |s| s.pending.contains(&key))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = waited;
            if timeout.timed_out() {
                return (
                    (
                        "503 Service Unavailable",
                        "application/json",
                        r#"{"error":"refresh still running; retry shortly"}"#.to_string(),
                    ),
                    None,
                );
            }
            if let Some(snapshot) = state.cache.get(&key)
                && snapshot.computed >= requested_at
                && snapshot.computed.elapsed() < ttl
            {
                let observation = Observation {
                    observed_at: snapshot.observed_at,
                    cached: true,
                    refreshing: false,
                };
                return (snapshot.response(), Some(observation));
            }
        }
        state.pending.insert(key.clone());
        drop(state);
        let guard = PendingGuard { store: self, key };
        let response = compute();
        let observation = self.record(&guard.key, &response);
        drop(guard);
        (response, observation)
    }

    /// Compute `route` for `partition` in the background, unless a
    /// computation for it is already running.
    pub(crate) fn warm(self: &Arc<Self>, route: &str, partition: &str, compute: Compute) {
        let Some((route, _)) = display_ttl(route) else {
            return;
        };
        self.load_partition(partition);
        let key: Key = (partition.to_string(), route);
        if self.lock().pending.insert(key.clone()) {
            self.refresh_in_background(key, compute);
        }
    }

    /// The routes whose snapshots are refreshed when the server starts.
    pub(crate) fn warmed_routes() -> &'static [&'static str] {
        PERSISTED
    }

    fn refresh_in_background(self: &Arc<Self>, key: Key, compute: Compute) {
        let store = Arc::clone(self);
        let task_key = key.clone();
        let spawned = std::thread::Builder::new()
            .name("lean-ctx-display-refresh".into())
            .spawn(move || {
                let guard = PendingGuard {
                    store: &store,
                    key: task_key,
                };
                let response = compute();
                store.record(&guard.key, &response);
            });
        if spawned.is_err() {
            drop(PendingGuard { store: self, key });
        }
    }

    /// Keep a successful response as the route's snapshot.
    fn record(&self, key: &Key, response: &RouteResponse) -> Option<Observation> {
        if response.0 != "200 OK" {
            return None;
        }
        let snapshot = Snapshot {
            computed: Instant::now(),
            observed_at: SystemTime::now(),
            content_type: response.1,
            body: response.2.clone(),
        };
        let observed_at = snapshot.observed_at;
        let mut state = self.lock();
        state.cache.insert(key.clone(), snapshot);
        let persisted = PERSISTED
            .contains(&key.1)
            .then(|| self.persisted_view(&state, &key.0));
        drop(state);
        if let Some(view) = persisted {
            self.persist(&key.0, &view);
        }
        Some(Observation {
            observed_at,
            cached: false,
            refreshing: false,
        })
    }

    fn persisted_view(&self, state: &State, partition: &str) -> serde_json::Value {
        let mut snapshots = BTreeMap::new();
        for route in PERSISTED {
            if let Some(snapshot) = state.cache.get(&(partition.to_string(), *route)) {
                let observed_ms = snapshot
                    .observed_at
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                snapshots.insert(
                    *route,
                    serde_json::json!({
                        "observed_at_ms": observed_ms,
                        "content_type": snapshot.content_type,
                        "body": snapshot.body,
                    }),
                );
            }
        }
        serde_json::json!({
            "schema": SNAPSHOT_SCHEMA,
            "engine": env!("CARGO_PKG_VERSION"),
            "snapshots": snapshots,
        })
    }

    fn persist(&self, partition: &str, view: &serde_json::Value) {
        let Some(file) = self.file(partition) else {
            return;
        };
        let Some(parent) = file.parent() else {
            return;
        };
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        if builder.create(parent).is_err() {
            return;
        }
        let Ok(bytes) = serde_json::to_vec(view) else {
            return;
        };
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(std::fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let permissions = None;
        if let Err(error) =
            crate::core::atomic_fs::try_atomic_write(&file, &bytes, permissions.as_ref())
        {
            tracing::debug!("dashboard display snapshot not persisted: {error}");
        }
    }

    /// Load the partition's persisted snapshots once, keeping those still
    /// inside their `STALE_OK` window and written by this engine version.
    fn load_partition(&self, partition: &str) {
        if !self.lock().loaded.insert(partition.to_string()) {
            return;
        }
        let Some(file) = self.file(partition) else {
            return;
        };
        let Ok(raw) = std::fs::read(&file) else {
            return;
        };
        let Ok(saved) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            return;
        };
        if saved["schema"] != SNAPSHOT_SCHEMA || saved["engine"] != env!("CARGO_PKG_VERSION") {
            return;
        }
        let now = SystemTime::now();
        let mut restored = Vec::new();
        for route in PERSISTED {
            let entry = &saved["snapshots"][*route];
            let (Some(observed_ms), Some(body)) =
                (entry["observed_at_ms"].as_u64(), entry["body"].as_str())
            else {
                continue;
            };
            if entry["content_type"] != "application/json" {
                continue;
            }
            let observed_at = UNIX_EPOCH + Duration::from_millis(observed_ms);
            let Ok(age) = now.duration_since(observed_at) else {
                continue;
            };
            if stale_ok(route).is_none_or(|window| age > window) {
                continue;
            }
            let Some(computed) = Instant::now().checked_sub(age) else {
                continue;
            };
            restored.push((
                *route,
                Snapshot {
                    computed,
                    observed_at,
                    content_type: "application/json",
                    body: body.to_string(),
                },
            ));
        }
        let mut state = self.lock();
        for (route, snapshot) in restored {
            state
                .cache
                .entry((partition.to_string(), route))
                .or_insert(snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting(body: &'static str) -> (Compute, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let compute: Compute = Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            ("200 OK", "application/json", body.to_string())
        });
        (compute, calls)
    }

    fn age(store: &DisplaySnapshots, partition: &str, route: &'static str, by: Duration) {
        let mut state = store.lock();
        let snapshot = state
            .cache
            .get_mut(&(partition.to_string(), route))
            .expect("snapshot");
        snapshot.computed = Instant::now().checked_sub(by).unwrap();
    }

    #[test]
    fn within_the_ttl_a_read_is_served_from_the_snapshot_without_recomputing() {
        let store = Arc::new(DisplaySnapshots::new(None));
        let (compute, calls) = counting(r#"{"n":1}"#);

        let (first, fresh) = store.serve("/api/stats", "p1", &compute);
        let (second, cached) = store.serve("/api/stats", "p1", &compute);

        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let fresh = fresh.unwrap();
        let cached = cached.unwrap();
        assert!(!fresh.cached && cached.cached && !cached.refreshing);
        assert_eq!(fresh.observed_at, cached.observed_at);
        assert!(cached.headers().contains("X-LeanCTX-Snapshot: cached\r\n"));
        assert!(fresh.headers().contains("X-LeanCTX-Snapshot: fresh\r\n"));

        // Past the 3 s TTL (no STALE_OK for stats) the read is recomputed.
        age(&store, "p1", "/api/stats", Duration::from_secs(4));
        let (_, again) = store.serve("/api/stats", "p1", &compute);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!again.unwrap().cached);

        // Another project never gets this project's snapshot.
        let (_, other) = store.serve("/api/stats", "p2", &compute);
        assert!(!other.unwrap().cached);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_stale_ok_read_answers_at_once_and_refreshes_once_in_the_background() {
        let store = Arc::new(DisplaySnapshots::new(None));
        let (compute, calls) = counting(r#"{"roi":{}}"#);
        store.serve("/api/roi", "p1", &compute);
        age(&store, "p1", "/api/roi", Duration::from_secs(60));

        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let slow_calls = Arc::new(AtomicUsize::new(0));
        let slow: Compute = {
            let gate = gate.clone();
            let slow_calls = slow_calls.clone();
            Arc::new(move || {
                slow_calls.fetch_add(1, Ordering::SeqCst);
                let (open, cv) = &*gate;
                let mut open = open.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
                (
                    "200 OK",
                    "application/json",
                    r#"{"roi":{"new":true}}"#.to_string(),
                )
            })
        };

        let mut readers = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let slow = slow.clone();
            readers.push(std::thread::spawn(move || {
                store.serve("/api/roi", "p1", &slow)
            }));
        }
        for reader in readers {
            let (response, observation) = reader.join().unwrap();
            let observation = observation.unwrap();
            assert_eq!(response.2, r#"{"roi":{}}"#, "the old snapshot, at once");
            assert!(observation.cached && observation.refreshing);
            assert!(
                observation
                    .headers()
                    .contains("X-LeanCTX-Refreshing: 1\r\n")
            );
        }
        {
            let (open, cv) = &*gate;
            *open.lock().unwrap() = true;
            cv.notify_all();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while store
            .lock()
            .pending
            .contains(&("p1".to_string(), "/api/roi"))
        {
            assert!(Instant::now() < deadline, "refresh never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(slow_calls.load(Ordering::SeqCst), 1, "single flight");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let (response, observation) = store.serve("/api/roi", "p1", &compute);
        assert_eq!(response.2, r#"{"roi":{"new":true}}"#);
        assert!(!observation.unwrap().refreshing);
    }

    #[test]
    fn persisted_snapshots_are_private_and_warm_a_new_server() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(SNAPSHOT_DIR);
        let store = Arc::new(DisplaySnapshots::new(Some(root.clone())));
        let (compute, _) = counting(r#"{"value":42}"#);
        store.serve("/api/value", "abc123", &compute);
        store.serve("/api/stats", "abc123", &compute);

        let file = root.join("abc123").join(SNAPSHOT_FILE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(
            saved["snapshots"]["/api/stats"].is_null(),
            "stats are not kept"
        );

        // A restarted server answers from disk without computing.
        let restarted = Arc::new(DisplaySnapshots::new(Some(root.clone())));
        let (never, never_calls) = counting("{}");
        let (response, observation) = restarted.serve("/api/value", "abc123", &never);
        assert_eq!(response.2, r#"{"value":42}"#);
        assert!(observation.unwrap().cached);
        assert_eq!(never_calls.load(Ordering::SeqCst), 0);

        // A snapshot past STALE_OK, or a partition name that is not one, is
        // never used.
        let mut old = saved.clone();
        old["snapshots"]["/api/value"]["observed_at_ms"] = serde_json::json!(1_000);
        std::fs::write(&file, serde_json::to_vec(&old).unwrap()).unwrap();
        let cold = Arc::new(DisplaySnapshots::new(Some(root.clone())));
        let (_, observation) = cold.serve("/api/value", "abc123", &never);
        assert!(!observation.unwrap().cached);
        assert!(cold.file("../escape").is_none());
    }

    #[test]
    fn a_failed_read_is_not_kept_as_a_snapshot() {
        let store = Arc::new(DisplaySnapshots::new(None));
        let failing: Compute =
            Arc::new(|| ("500 Internal Server Error", "application/json", "{}".into()));
        let (_, observation) = store.serve("/api/health", "p", &failing);
        assert!(observation.is_none());
        assert!(store.lock().cache.is_empty());
        assert!(store.lock().pending.is_empty());
    }
}
