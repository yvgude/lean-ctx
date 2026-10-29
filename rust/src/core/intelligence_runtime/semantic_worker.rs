// SPDX-License-Identifier: Apache-2.0
//! Host supervisor of the warm semantic-detector worker.
//!
//! One worker per Engine process: a child of the runtime binary authenticated
//! from the signed archive (`health::prepare`, the same start as every other
//! runtime invocation), started with `--semantic-serve`, speaking one JSON
//! document per line over its standard input and output. It holds the model
//! in memory, so an admission pays for inference only. No pid file, no socket
//! and nothing shared between processes: the worker belongs to this process
//! and ends with it, at the latest when its input closes.
//!
//! The protocol carries no request id. Pairing is by order alone and is kept
//! by one rule: a request line is written only when no answer is outstanding.
//! One caller holds the worker at a time; an answer that did not arrive in
//! time stays owed, and the next request is withheld until that line has been
//! read and discarded, or the worker is stopped when it does not come.
//!
//! The lock below guards state only. It is never held while a caller waits
//! for a line or while a package is authenticated; it nests no other lock.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use super::semantic_detectors::NotInspected;
use crate::core::config::IntelligenceRuntimeConfig;

/// Seconds without a request after which the worker ends itself and releases
/// the model (0.7 GiB and more). The runtime accepts 10..=3600; ten minutes
/// keeps an interactive session warm and frees the memory of an idle one.
pub(super) const IDLE_SECONDS: u64 = 600;
/// After an answer missed its deadline, how long the late line may still take
/// before the worker counts as unresponsive and is stopped. The runtime ends
/// its inference at the request deadline, so the line normally follows at once.
const DRAIN_BOUND: Duration = Duration::from_secs(3);
/// Waits before the first, second and third restart after a failure. A fourth
/// failure in a row leaves the detector unavailable for the process lifetime;
/// an answered request ends the row.
const RESTART_BACKOFF: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_mins(2),
];
/// Callers that may wait for a worker another caller holds. Any further one
/// is answered at once as not inspected.
const MAX_WAITING: usize = 1;
/// Largest request line the runtime reads.
const MAX_REQUEST_LINE: usize = 1024 * 1024;
/// Largest line accepted from the worker: its frame bound plus a margin. A
/// longer or unterminated line is a protocol error and stops the worker.
const MAX_WORKER_LINE: u64 = 1024 * 1024 + 4096;
/// Lines the reader may hold ahead of the supervisor: the ready line or one
/// answer is all a correct worker has outstanding.
const EVENT_QUEUE: usize = 4;
/// Bytes of the worker's error stream that are kept; the rest is discarded.
const STDERR_PREFIX: usize = 256;

/// What the reader of a worker's output reports.
pub(super) enum Event {
    /// One terminated line, terminator included.
    Line(Vec<u8>),
    /// The output ended.
    Closed,
    /// A line over the bound, an unterminated line or a read error.
    Violation,
}

/// The supervisor's hold on one started worker.
pub(super) trait Link: Send {
    /// Hand one request line to the worker without blocking; `false` when it
    /// cannot be taken.
    fn send(&mut self, line: Vec<u8>) -> bool;
    /// Stop the worker and collect it. Repeated calls do nothing.
    fn stop(&mut self);
}

pub(super) struct Started {
    pub(super) link: Box<dyn Link>,
    pub(super) events: Receiver<Event>,
    /// The contract version negotiated from the authenticated package.
    pub(super) version: &'static str,
    /// The package takes the Hive runtime lease (`runtime_lease` v1 or v2).
    pub(super) takes_lease: bool,
    /// The lease last handed to the worker.
    pub(super) lease: Option<String>,
}

/// Starts a worker for the given runtime settings. Called on its own thread:
/// it authenticates the package and may take as long as that takes.
pub(super) trait Launcher: Send + Sync {
    fn launch(&self, config: &IntelligenceRuntimeConfig) -> Option<Started>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    /// No worker. The next inspection starts one.
    Cold,
    /// Started; the ready line has not been read yet.
    Warming,
    /// Loaded and answering.
    Warm,
    /// Stopped after a failure; restarted after a wait, or not at all.
    Failed,
}

impl Phase {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warming => "warming",
            Self::Warm => "warm",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Timing {
    pub(super) idle: Duration,
    pub(super) drain: Duration,
    pub(super) backoff: [Duration; 3],
}

impl Timing {
    const PRODUCT: Self = Self {
        idle: Duration::from_secs(IDLE_SECONDS),
        drain: DRAIN_BOUND,
        backoff: RESTART_BACKOFF,
    };
}

struct Session {
    link: Box<dyn Link>,
    events: Receiver<Event>,
    version: &'static str,
    takes_lease: bool,
    lease: Option<String>,
    /// Start, or the moment the last request was handed over: never later
    /// than the moment the worker began counting its idle time.
    idle_since: Instant,
    /// An answer is outstanding from a request that gave up; until when the
    /// line may still arrive.
    owed: Option<Instant>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.link.stop();
    }
}

struct Inner {
    /// Digest of the runtime settings the worker was started for.
    key: Option<[u8; 32]>,
    /// Changes with every start, stop and failure: whoever started or holds a
    /// worker of an earlier generation finds it retired.
    generation: u64,
    phase: Phase,
    session: Option<Session>,
    /// A caller holds the session outside the lock.
    busy: bool,
    waiting: usize,
    /// Failures in a row.
    failures: usize,
    retry_at: Option<Instant>,
    /// What the ready line named; empty unless warm.
    versions: Vec<String>,
}

type Answer = Result<(String, bool), NotInspected>;

pub(super) struct Supervisor {
    inner: Mutex<Inner>,
    freed: Condvar,
    launcher: Box<dyn Launcher>,
    timing: Timing,
}

impl Supervisor {
    pub(super) fn with(launcher: Box<dyn Launcher>, timing: Timing) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                key: None,
                generation: 0,
                phase: Phase::Cold,
                session: None,
                busy: false,
                waiting: 0,
                failures: 0,
                retry_at: None,
                versions: Vec::new(),
            }),
            freed: Condvar::new(),
            launcher,
            timing,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stop whatever runs and forget it, failures included.
    fn retire_locked(inner: &mut Inner, key: Option<[u8; 32]>) {
        inner.session = None;
        inner.generation = inner.generation.wrapping_add(1);
        inner.phase = Phase::Cold;
        inner.busy = false;
        inner.failures = 0;
        inner.retry_at = None;
        inner.versions.clear();
        inner.key = key;
    }

    /// The worker ended itself after its idle time: nothing failed.
    fn cool(inner: &mut Inner) {
        inner.session = None;
        inner.generation = inner.generation.wrapping_add(1);
        inner.phase = Phase::Cold;
        inner.versions.clear();
    }

    fn fail(&self, inner: &mut Inner, now: Instant) {
        inner.session = None;
        inner.generation = inner.generation.wrapping_add(1);
        inner.busy = false;
        inner.versions.clear();
        inner.phase = Phase::Failed;
        inner.retry_at = self
            .timing
            .backoff
            .get(inner.failures)
            .map(|wait| now + *wait);
        inner.failures = inner.failures.saturating_add(1);
    }

    /// Take in what the worker has written since the last look, without
    /// waiting. Skipped while a caller holds the session.
    fn observe(&self, inner: &mut Inner, now: Instant) {
        if inner.busy {
            return;
        }
        loop {
            let Some(session) = inner.session.as_mut() else {
                return;
            };
            let event = match session.events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => Event::Closed,
            };
            match (inner.phase, event) {
                (Phase::Warming, Event::Line(line)) => {
                    let Some(versions) = ready(&line, session.version) else {
                        tracing::warn!(
                            "semantic worker failed: its ready line does not name contract {}",
                            session.version
                        );
                        return self.fail(inner, now);
                    };
                    inner.versions = versions;
                    inner.phase = Phase::Warm;
                }
                // The late answer of a request that gave up: discarded.
                (Phase::Warm, Event::Line(_)) if session.owed.is_some() => session.owed = None,
                (Phase::Warm, Event::Closed)
                    if session.owed.is_none()
                        && now.saturating_duration_since(session.idle_since)
                            >= self.timing.idle =>
                {
                    return Self::cool(inner);
                }
                // An unrequested line, an early end, a protocol violation.
                (phase, event) => {
                    let what = match event {
                        Event::Line(_) => "an unrequested line",
                        Event::Closed => "its output ended",
                        Event::Violation => "a line outside the protocol",
                    };
                    tracing::warn!("semantic worker failed while {}: {what}", phase.as_str());
                    return self.fail(inner, now);
                }
            }
        }
        let overdue = inner
            .session
            .as_ref()
            .and_then(|session| session.owed)
            .is_some_and(|bound| now >= bound);
        if overdue {
            self.fail(inner, now);
        }
    }

    /// Start a worker on its own thread and return at once.
    fn start(self: &Arc<Self>, inner: &mut Inner, config: &IntelligenceRuntimeConfig) {
        inner.session = None;
        inner.generation = inner.generation.wrapping_add(1);
        inner.phase = Phase::Warming;
        inner.retry_at = None;
        inner.versions.clear();
        let generation = inner.generation;
        let supervisor = Arc::clone(self);
        let config = config.clone();
        let spawned = std::thread::Builder::new()
            .name("semantic-worker-start".to_owned())
            .spawn(move || {
                let session = supervisor.launcher.launch(&config).map(|started| Session {
                    link: started.link,
                    events: started.events,
                    version: started.version,
                    takes_lease: started.takes_lease,
                    lease: started.lease,
                    idle_since: Instant::now(),
                    owed: None,
                });
                let mut inner = supervisor.lock();
                if inner.generation != generation {
                    // Retired while starting: the session stops as it drops.
                    return;
                }
                match session {
                    Some(session) => inner.session = Some(session),
                    None => supervisor.fail(&mut inner, Instant::now()),
                }
            });
        if spawned.is_err() {
            self.fail(inner, Instant::now());
        }
    }

    /// One request under `deadline`. Never waits past it, and never for a
    /// start: a worker that is not warm answers `Warming` at once.
    pub(super) fn request(
        self: &Arc<Self>,
        config: &IntelligenceRuntimeConfig,
        key: [u8; 32],
        input: &str,
        deadline: Instant,
        deadline_unix_ms: u64,
    ) -> Answer {
        let mut inner = self.lock();
        if inner.key != Some(key) {
            // Other settings or another package: that worker is not asked.
            if inner.key.is_some() {
                tracing::info!("semantic worker retired: the runtime settings changed");
            }
            Self::retire_locked(&mut inner, Some(key));
            self.freed.notify_all();
        }
        let (mut session, generation) = loop {
            let now = Instant::now();
            self.observe(&mut inner, now);
            match inner.phase {
                Phase::Warm => {}
                Phase::Warming => return Err(NotInspected::Warming),
                Phase::Failed if inner.retry_at.is_none_or(|at| now < at) => {
                    return Err(NotInspected::Unavailable);
                }
                Phase::Cold | Phase::Failed => {
                    self.start(&mut inner, config);
                    return Err(NotInspected::Warming);
                }
            }
            if !inner.busy {
                let Some(session) = inner.session.take() else {
                    self.fail(&mut inner, now);
                    return Err(NotInspected::Unavailable);
                };
                inner.busy = true;
                break (session, inner.generation);
            }
            let wait = deadline.saturating_duration_since(now);
            if inner.waiting >= MAX_WAITING || wait.is_zero() {
                return Err(NotInspected::Unavailable);
            }
            inner.waiting += 1;
            let (guard, _) = self
                .freed
                .wait_timeout(inner, wait)
                .unwrap_or_else(PoisonError::into_inner);
            inner = guard;
            inner.waiting -= 1;
        };
        drop(inner);
        let (answer, intact) = exchange(
            &mut session,
            input,
            deadline,
            deadline_unix_ms,
            self.timing.drain,
        );
        let mut inner = self.lock();
        // A worker retired while it was held is not put back; it stops when
        // `session` drops below.
        if inner.generation == generation {
            inner.busy = false;
            if intact {
                if answer.is_ok() {
                    inner.failures = 0;
                }
                inner.session = Some(session);
            } else {
                self.fail(&mut inner, Instant::now());
            }
        }
        drop(inner);
        self.freed.notify_all();
        answer
    }

    /// Stop the worker; the next use starts a new one.
    pub(super) fn retire(&self) {
        let mut inner = self.lock();
        if inner.key.is_some() {
            tracing::info!("semantic worker retired: runtime switched off or package moved");
            Self::retire_locked(&mut inner, None);
            self.freed.notify_all();
        }
    }

    /// The state of the worker for these settings and, when warm, the
    /// contract versions it named. Starts nothing.
    pub(super) fn phase(&self, key: [u8; 32]) -> (Phase, Vec<String>) {
        let mut inner = self.lock();
        if inner.key != Some(key) {
            return (Phase::Cold, Vec::new());
        }
        self.observe(&mut inner, Instant::now());
        (inner.phase, inner.versions.clone())
    }
}

/// Send one request and wait for its answer. The flag is `false` when the
/// worker can no longer be used. A stale answer is consumed before anything
/// is written, so the line read after the request is the answer to it.
fn exchange(
    session: &mut Session,
    input: &str,
    deadline: Instant,
    deadline_unix_ms: u64,
    drain: Duration,
) -> (Answer, bool) {
    let unavailable = |intact: bool| (Err(NotInspected::Unavailable), intact);
    if let Some(bound) = session.owed {
        let wait = bound
            .min(deadline)
            .saturating_duration_since(Instant::now());
        match session.events.recv_timeout(wait) {
            Ok(Event::Line(_)) => session.owed = None,
            Err(RecvTimeoutError::Timeout) => return unavailable(Instant::now() < bound),
            Ok(Event::Closed | Event::Violation) | Err(RecvTimeoutError::Disconnected) => {
                return unavailable(false);
            }
        }
    }
    // A renewed lease reaches the worker before the request it licenses;
    // the admission line has no reply.
    if session.takes_lease {
        let current = super::lease::current_lease_b64();
        if let Some(lease) = current.as_deref()
            && current != session.lease
        {
            if !session.link.send(lease_line(lease)) {
                return unavailable(false);
            }
            session.lease = current;
        }
    }
    let Some(line) = request_line(session.version, deadline_unix_ms, input) else {
        return unavailable(true);
    };
    let now = Instant::now();
    let wait = deadline.saturating_duration_since(now);
    if wait.is_zero() {
        return unavailable(true);
    }
    session.idle_since = now;
    if !session.link.send(line) {
        return unavailable(false);
    }
    match session.events.recv_timeout(wait) {
        Ok(Event::Line(line)) => match reply(&line) {
            Reply::Output(output) => (Ok((output, session.version == "1.1.0")), true),
            Reply::Refused => unavailable(true),
            Reply::Invalid => unavailable(false),
        },
        Err(RecvTimeoutError::Timeout) => {
            session.owed = Some(Instant::now() + drain);
            unavailable(true)
        }
        Ok(Event::Closed | Event::Violation) | Err(RecvTimeoutError::Disconnected) => {
            unavailable(false)
        }
    }
}

/// The admission line that hands the worker the Hive runtime lease.
fn lease_line(lease: &str) -> Vec<u8> {
    let mut line = serde_json::json!({ "lease_b64": lease })
        .to_string()
        .into_bytes();
    line.push(b'\n');
    line
}

fn request_line(version: &str, deadline_unix_ms: u64, input: &str) -> Option<Vec<u8>> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "version": version,
        "deadline_unix_ms": deadline_unix_ms,
        "input": input,
    }))
    .ok()?;
    line.push(b'\n');
    (line.len() <= MAX_REQUEST_LINE).then_some(line)
}

/// The versions of a valid ready line that includes the negotiated one.
fn ready(line: &[u8], negotiated: &str) -> Option<Vec<String>> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ready {
        schema_version: u32,
        status: String,
        versions: Vec<String>,
    }
    let ready: Ready = serde_json::from_slice(line).ok()?;
    let valid = {
        let named: Vec<&str> = ready.versions.iter().map(String::as_str).collect();
        ready.schema_version == 1
            && ready.status == "ready"
            && matches!(named.as_slice(), ["1.1.0"] | ["1.0.0", "1.1.0"])
            && named.contains(&negotiated)
    };
    valid.then_some(ready.versions)
}

enum Reply {
    /// The answer JSON of the contract; validated by the caller.
    Output(String),
    Refused,
    Invalid,
}

fn reply(line: &[u8]) -> Reply {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Wire {
        status: String,
        #[serde(default)]
        output: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    }
    let Ok(wire) = serde_json::from_slice::<Wire>(line) else {
        return Reply::Invalid;
    };
    match (wire.status.as_str(), wire.output, wire.reason.as_deref()) {
        ("ok", Some(output), None) => Reply::Output(output),
        ("refused", None, Some("entitlement" | "unsupported" | "input" | "inference")) => {
            Reply::Refused
        }
        _ => Reply::Invalid,
    }
}

/// The runtime binary as a child process.
struct ProcessLauncher;

struct ProcessLink {
    child: Option<std::process::Child>,
    lines: Option<SyncSender<Vec<u8>>>,
    /// The private directory the authenticated binary runs from.
    prepared: Option<super::health::Prepared>,
}

impl Link for ProcessLink {
    fn send(&mut self, line: Vec<u8>) -> bool {
        self.lines
            .as_ref()
            .is_some_and(|lines| lines.try_send(line).is_ok())
    }

    fn stop(&mut self) {
        // Closing the queue ends the writer, which closes the worker's input.
        self.lines = None;
        if let Some(mut child) = self.child.take() {
            // Killed, then collected: no zombie remains. The kill cannot be
            // refused by the child, so the wait ends with its teardown.
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(prepared) = self.prepared.take() {
            let _ = prepared.close();
        }
    }
}

impl Drop for ProcessLink {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&self, config: &IntelligenceRuntimeConfig) -> Option<Started> {
        // The step a start stopped at is the only thing an operator can act
        // on; it is a fixed word, never a path or content.
        let step = std::cell::Cell::new("trust_key");
        let started = Self::launch_at(config, &step);
        if started.is_none() {
            tracing::warn!("semantic worker not started: step {}", step.get());
        }
        started
    }
}

impl ProcessLauncher {
    fn launch_at(
        config: &IntelligenceRuntimeConfig,
        step: &std::cell::Cell<&'static str>,
    ) -> Option<Started> {
        use std::process::Stdio;
        let root = std::path::Path::new(&config.root);
        let trust = super::trust_key(&config.trust_key_hex).ok()?;
        step.set("package_authentication");
        let package =
            super::install::verified_active(root, &config.manifest_sha256, &trust).ok()?;
        step.set("contract_version");
        let version = super::health::semantic_detectors_version(&package)?;
        // The same start as every runtime invocation: the binary comes from
        // the signed archive, the environment is cleared, the working
        // directory is private.
        step.set("prepare");
        let (prepared, mut command) = super::health::prepare(&package, root).ok()?;
        // The installed model set and the descriptor digest of the signed
        // manifest, named as for every start that is given the set. Without
        // an admitted set there is nothing to serve and nothing is started.
        step.set("model_set");
        super::health::name_semantic_models(&package, &mut command)
            .ok()
            .flatten()?;
        step.set("lease_configuration");
        command
            .arg("--semantic-serve")
            .arg(IDLE_SECONDS.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The lease configuration is named exactly as for a one-shot start;
        // only the private peer interprets it.
        // Without a commercial licence the Hive lease is the claim: it is
        // the worker's first input line, before it loads anything.
        let takes_lease = super::session::takes_lease(&package);
        let lease = takes_lease.then(super::lease::current_lease_b64).flatten();
        #[cfg(any(unix, windows))]
        if package.description["entitlement_required"] == true {
            match super::license::configuration_path(root, &config.manifest_sha256, &trust) {
                Ok(configuration) => {
                    command.env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration);
                }
                Err(_) if lease.is_some() => {}
                Err(_) => return None,
            }
        }
        step.set("spawn");
        let child = command.spawn().ok()?;
        step.set("pipes");
        // From here every early return stops and collects the child.
        let mut link = ProcessLink {
            child: Some(child),
            lines: None,
            prepared: Some(prepared),
        };
        let child = link.child.as_mut()?;
        let input = child.stdin.take()?;
        let output = child.stdout.take()?;
        let errors = child.stderr.take()?;
        // Room for a lease line and the request it licenses.
        let (lines, queued) = sync_channel::<Vec<u8>>(2);
        let (events, incoming) = sync_channel(EVENT_QUEUE);
        on_thread("semantic-worker-input", move || write_lines(input, &queued))?;
        on_thread("semantic-worker-output", move || {
            read_lines(output, &events);
        })?;
        on_thread("semantic-worker-errors", move || drain_errors(errors))?;
        link.lines = Some(lines);
        if let Some(lease) = lease.as_deref()
            && !link.send(lease_line(lease))
        {
            return None;
        }
        Some(Started {
            link: Box::new(link),
            events: incoming,
            version,
            takes_lease,
            lease,
        })
    }
}

fn on_thread(name: &str, work: impl FnOnce() + Send + 'static) -> Option<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .ok()
        .map(drop)
}

/// Writes on its own thread, so a worker that stops reading holds up no
/// admission. Ends, and closes the worker's input, when the queue closes.
fn write_lines(mut input: impl Write, queued: &Receiver<Vec<u8>>) {
    for line in queued {
        if input.write_all(&line).and_then(|()| input.flush()).is_err() {
            return;
        }
    }
}

/// One event per line; the first end or violation is the last event.
fn read_lines(output: impl Read, events: &SyncSender<Event>) {
    let mut reader = BufReader::new(output);
    loop {
        let mut line = Vec::new();
        let read = (&mut reader)
            .take(MAX_WORKER_LINE + 1)
            .read_until(b'\n', &mut line);
        let event = match read {
            Ok(0) => Event::Closed,
            Ok(_) if line.last() == Some(&b'\n') => Event::Line(line),
            _ => Event::Violation,
        };
        let last = !matches!(event, Event::Line(_));
        if events.send(event).is_err() || last {
            return;
        }
    }
}

/// The worker writes at most one content-free line there. It is read to its
/// end so the worker never blocks on it; only a bounded, printable prefix of
/// the first line is ever shown.
fn drain_errors(mut errors: impl Read) {
    let mut prefix = [0_u8; STDERR_PREFIX];
    let mut kept = 0;
    while kept < prefix.len() {
        match errors.read(&mut prefix[kept..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => kept += read,
        }
    }
    let _ = std::io::copy(&mut errors, &mut std::io::sink());
    if kept > 0 {
        let line: String = prefix[..kept]
            .iter()
            .take_while(|byte| **byte != b'\n')
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '?'
                }
            })
            .collect();
        // The runtime's one content-free line (why it would not serve); a
        // bounded printable prefix, shown so that a worker that cannot start
        // is not a silent "unavailable".
        tracing::warn!("semantic worker ended: {line}");
    }
}

static GLOBAL: OnceLock<Arc<Supervisor>> = OnceLock::new();

fn key_of(config: &IntelligenceRuntimeConfig) -> Option<[u8; 32]> {
    let settings = serde_json::to_vec(config).ok()?;
    Some(*blake3::hash(&settings).as_bytes())
}

/// Ask this process's worker; it is started in the background when none runs.
pub(super) fn request(
    config: &IntelligenceRuntimeConfig,
    input: &str,
    deadline: Instant,
    deadline_unix_ms: u64,
) -> Answer {
    let key = key_of(config).ok_or(NotInspected::Unavailable)?;
    GLOBAL
        .get_or_init(|| Supervisor::with(Box::new(ProcessLauncher), Timing::PRODUCT))
        .request(config, key, input, deadline, deadline_unix_ms)
}

/// Stop this process's worker, if any.
pub(super) fn retire() {
    if let Some(supervisor) = GLOBAL.get() {
        supervisor.retire();
    }
}

/// The state of this process's worker for these settings: `cold`, `warming`,
/// `warm` or `failed`, and the versions a warm one named. Starts nothing.
pub(super) fn state(config: &IntelligenceRuntimeConfig) -> (&'static str, Vec<String>) {
    let observed = key_of(config)
        .zip(GLOBAL.get())
        .map(|(key, supervisor)| supervisor.phase(key));
    match observed {
        Some((phase, versions)) => (phase.as_str(), versions),
        None => (Phase::Cold.as_str(), Vec::new()),
    }
}
