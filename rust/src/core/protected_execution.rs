// SPDX-License-Identifier: Apache-2.0
//! Supervised execution channel for protected Codex sessions.
//!
//! macOS refuses a nested `sandbox_apply` as soon as the *outer* profile is
//! anything but `(allow default)`: one `(deny mach-task-read)` — or a single
//! unrelated file-read denial — in the whole-MCP profile makes every inner
//! `sandbox-exec` fail with `Operation not permitted`. This was measured on the
//! running system, not inferred. A protected MCP server therefore cannot wrap
//! its own command children in the launcher's store profile, and the previous
//! per-child `sandbox-exec` prefix could only ever fail closed — which is a
//! broken shell, not a security result.
//!
//! The launcher's wrapper process already runs *outside* the MCP sandbox and is
//! the MCP server's parent. This module turns it into a narrowly scoped
//! supervisor. One anonymous `UnixStream` pair is inherited by the MCP child at
//! a fixed descriptor, sealed `FD_CLOEXEC` before any thread, TCC probe or tool
//! can observe it, and never reaches a command child. The supervisor is the only
//! process that starts command children, and it applies the complete
//! launcher-owned profile once, at spawn, from an unsandboxed parent.
//!
//! What this file is responsible for:
//! * the combined profile is launcher-owned — a request can never select,
//!   weaken or drop it; a `Program` request may only *narrow* it;
//! * a missing, invalid or closed channel fails closed — there is no
//!   unsandboxed fallback shell;
//! * frames, live-output snapshots, wall time and concurrency are bounded;
//! * diagnostics carry no command text, environment value or file content.
//!
//! Not in scope, deliberately: no named listener, no TCP service, no daemon
//! delegation, no durable key. The channel is a process capability that dies
//! with the wrapper.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

/// Argument the launcher's wrapper passes to the sandboxed MCP child.
pub(crate) const EXEC_FD_FLAG: &str = "--protected-exec-fd";
/// Fixed descriptor the wrapper dups the MCP end onto. `3` stays reserved for
/// the selected-GitLab credential channel, which is read and closed earlier.
pub(crate) const CHILD_EXEC_FD: i32 = 4;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// Every profile the launcher pins starts here; a narrowing request may not.
const LAUNCH_PROFILE_HEADER: &str = "(version 1)\n(allow default)\n";

/// Frame ceiling for both directions. Large enough for one capped shell capture
/// plus its framing, small enough that a malformed length is rejected outright.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
/// Concurrent command children one session may hold open. Foreground plus the
/// background jobs `ctx_shell` actually keeps alive; beyond this a request is
/// refused rather than queued behind an unbounded backlog.
const MAX_INFLIGHT: usize = 8;
/// Absolute supervisor-side lifetime for a single request, independent of the
/// executor's own idle/wall budget.
const MAX_REQUEST_WALL: Duration = Duration::from_hours(4);
/// Cadence at which the supervisor mirrors a running child's captured output.
const LIVE_INTERVAL: Duration = Duration::from_millis(250);
/// Client poll granularity; also how fast a cancellation reaches the wire.
const CLIENT_TICK: Duration = Duration::from_millis(50);
const CHANNEL_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// Grace period for in-flight children after the MCP server disconnects.
const TEARDOWN_GRACE: Duration = Duration::from_secs(5);

const PROTOCOL_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// Wire protocol
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    id: u64,
    body: Body,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Body {
    /// A model-directed shell command. The profile is never part of the
    /// request: the supervisor uses the one the launcher pinned.
    Shell {
        command: String,
        cwd: String,
        env: BTreeMap<String, String>,
        timeout_ms: Option<u64>,
        idle_keyed: bool,
    },
    /// `ctx_execute`'s own, narrower read/write/network profile. The supervisor
    /// composes it with the launch denials, which are appended last.
    Program {
        constrained: bool,
        read_paths: Vec<String>,
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        timeout_secs: u64,
        cwd: Option<String>,
    },
    /// Cooperative cancellation for an already accepted request.
    Cancel,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u8,
    id: u64,
    event: Event,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum Event {
    /// Progress snapshot of the captured output so far; advisory only.
    Live {
        text: String,
    },
    ShellExit {
        text: String,
        code: i32,
    },
    ProgramExit {
        stdout: String,
        stderr: String,
        code: i32,
    },
    /// The request never produced a child, or the channel refused it. Carries a
    /// fixed reason, never the request's own content.
    Failed {
        message: String,
    },
}

fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::other("frame exceeds the channel limit"));
    }
    stream.write_all(&(payload.len() as u32).to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()
}

/// `Ok(None)` is a clean end of stream. A truncated frame is indistinguishable
/// from a peer that died mid-write, and both mean the same thing here: the
/// channel is gone and every caller must fail closed.
fn read_frame(stream: &mut UnixStream) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0_u8; 4];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::other("frame exceeds the channel limit"));
    }
    let mut body = vec![0_u8; len];
    match stream.read_exact(&mut body) {
        Ok(()) => Ok(Some(body)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Launcher-owned profile
// ---------------------------------------------------------------------------

/// The immutable boundary the supervisor applies. Built once, in the wrapper,
/// from the rules the launcher pinned into the environment — never from a
/// request, an `extra_env` entry or a writable file.
#[derive(Clone)]
pub(crate) struct LaunchProfile {
    /// Complete profile for a shell child.
    shell: String,
    /// The denial lines alone, for composing onto a narrower caller profile.
    denials: String,
}

impl LaunchProfile {
    /// Fails closed: a wrapper that cannot reproduce the pinned boundary
    /// supervises nothing.
    pub(crate) fn from_pinned(control_profile: &str) -> Result<Self, String> {
        let prefix = crate::cli::protected_child_prefix()?
            .ok_or("supervised execution requires a protected session")?;
        let mut shell = match prefix.as_slice() {
            [program, flag, profile] if program == SANDBOX_EXEC && flag == "-p" => profile.clone(),
            _ => return Err("unexpected protected launch rules".into()),
        };
        if !shell.starts_with(LAUNCH_PROFILE_HEADER) {
            return Err("protected launch rules have an unexpected header".into());
        }
        // A supervisor child does not inherit the MCP's control-file sandbox.
        // Apply those rules explicitly, after every store/build exception.
        let controls = control_profile
            .strip_prefix(LAUNCH_PROFILE_HEADER)
            .ok_or("protected control profile has an unexpected header")?;
        for line in controls.lines().filter(|line| !line.is_empty()) {
            if !line.starts_with("(deny ")
                && line != "(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))"
            {
                return Err("protected control profile contains an unsupported grant".into());
            }
        }
        shell.push_str(controls);
        if shell.len() > 128 * 1024 {
            return Err("combined protected launch profile exceeds the limit".into());
        }
        let mut denials = String::new();
        for line in shell
            .strip_prefix(LAUNCH_PROFILE_HEADER)
            .ok_or("protected launch profile has an unexpected header")?
            .lines()
            .filter(|line| !line.is_empty())
        {
            if line.starts_with("(deny ") && line.ends_with(')') {
                denials.push_str(line);
                denials.push('\n');
            } else if !line.starts_with("(allow ") || !line.ends_with(')') {
                return Err("protected launch profile contains an unsupported rule".into());
            }
        }
        if denials.is_empty() {
            return Err("protected launch rules are not a pure denial set".into());
        }
        Ok(Self { shell, denials })
    }

    pub(crate) fn shell_profile(&self) -> &str {
        &self.shell
    }

    /// Build the restrictive program profile from typed paths. Requests never
    /// contain executable Seatbelt rules. Launch denials are appended last so
    /// a requested read root cannot re-open a control file or the store.
    fn compose(&self, read_paths: &[String], program: &str) -> Result<String, String> {
        if read_paths.len() > 64
            || read_paths
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(program))
                .any(|path| {
                    !std::path::Path::new(path).is_absolute() || path.chars().any(char::is_control)
                })
        {
            return Err("supervised program paths must be bounded and absolute".into());
        }
        let paths: Vec<&std::path::Path> = read_paths.iter().map(std::path::Path::new).collect();
        let requested = crate::core::sandbox_seatbelt::seatbelt_profile(&paths, program);
        if requested.len() + self.denials.len() > MAX_FRAME_BYTES {
            return Err("supervised program profile exceeds the channel limit".into());
        }
        let mut composed = String::with_capacity(requested.len() + self.denials.len() + 1);
        composed.push_str(&requested);
        if !composed.ends_with('\n') {
            composed.push('\n');
        }
        composed.push_str(&self.denials);
        Ok(composed)
    }

    #[cfg(test)]
    fn synthetic(denials: &str) -> Self {
        Self {
            shell: format!("{LAUNCH_PROFILE_HEADER}{denials}"),
            denials: denials.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Launch mode
// ---------------------------------------------------------------------------

thread_local! {
    /// Set only on a supervisor worker thread: the fixed profile this thread's
    /// child must run under. Its presence is what makes the shared executor take
    /// the broker-private path instead of dispatching back to the client.
    static BROKER_PROFILE: std::cell::RefCell<Option<String>> = const {
        std::cell::RefCell::new(None)
    };
}

fn broker_profile() -> Option<String> {
    BROKER_PROFILE.with(|slot| slot.borrow().clone())
}

fn with_broker_profile<T>(profile: String, run: impl FnOnce() -> T) -> T {
    BROKER_PROFILE.with(|slot| *slot.borrow_mut() = Some(profile));
    let result = run();
    BROKER_PROFILE.with(|slot| *slot.borrow_mut() = None);
    result
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn with_test_supervisor<T>(run: impl FnOnce() -> T) -> T {
    let prefix = crate::cli::protected_child_prefix().unwrap().unwrap();
    with_broker_profile(prefix[2].clone(), run)
}

pub(crate) enum Mode {
    /// Community session: the shared executor runs the child directly.
    Direct,
    /// Supervisor worker thread: apply this fixed profile at spawn. Legal only
    /// outside the MCP sandbox, which is exactly where the supervisor runs.
    Sandboxed(String),
    /// Protected MCP server: hand the request to the supervisor.
    Brokered(Arc<Client>),
}

/// How the calling thread must start a command child.
///
/// Fails closed in one case that matters: a process carrying the protected
/// launch authority with no usable channel runs nothing at all. It never
/// degrades to an unsupervised child.
pub(crate) fn launch_mode() -> Result<Mode, String> {
    if let Some(profile) = broker_profile() {
        return Ok(Mode::Sandboxed(profile));
    }
    if let Some(client) = client() {
        if client.is_closed() {
            return Err(
                "the protected execution channel closed; restart `lean-ctx codex-protected`".into(),
            );
        }
        return Ok(Mode::Brokered(client));
    }
    if crate::cli::protected_child_prefix()?.is_some() {
        return Err(
            "protected session without a supervised execution channel; restart \
             `lean-ctx codex-protected`"
                .into(),
        );
    }
    Ok(Mode::Direct)
}

/// Program and leading arguments for a broker-private child.
pub(crate) fn sandbox_prefix(profile: &str) -> Vec<String> {
    vec![
        SANDBOX_EXEC.to_string(),
        "-p".to_string(),
        profile.to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Client — the protected MCP server's end
// ---------------------------------------------------------------------------

static CLIENT: OnceLock<Arc<Client>> = OnceLock::new();

pub(crate) struct Client {
    writer: Mutex<UnixStream>,
    inflight: Mutex<std::collections::HashMap<u64, mpsc::SyncSender<Event>>>,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
}

pub(crate) fn client() -> Option<Arc<Client>> {
    CLIENT.get().cloned()
}

/// Adopt the descriptor the wrapper inherited to this process. Runs from the
/// early CLI dispatch, before logging, telemetry, threads or tools exist.
///
/// Validation mirrors the credential channel: an anonymous, connected stream
/// socket pair and nothing else, sealed `FD_CLOEXEC` so no command child can
/// ever inherit the capability.
pub(crate) fn install_client(fd: i32) -> Result<(), String> {
    use std::os::fd::FromRawFd;

    if fd <= 2 {
        return Err("invalid protected execution descriptor".into());
    }
    let mut kind: libc::c_int = 0;
    let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: kind/len are initialized writable values of the sizes supplied.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut len,
        )
    } != 0
        || kind != libc::SOCK_STREAM
    {
        return Err("supervised execution requires an anonymous stream socket".into());
    }
    // SAFETY: the early CLI receiver owns the inherited descriptor exactly once,
    // before other code can use it; getsockopt verified it is open.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    if !stream
        .local_addr()
        .is_ok_and(|address| address.is_unnamed())
        || !stream.peer_addr().is_ok_and(|address| address.is_unnamed())
    {
        return Err("supervised execution requires an anonymous connected socket pair".into());
    }
    // SAFETY: stream owns the open descriptor and F_SETFD takes integer flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err("cannot seal the protected execution descriptor".into());
    }
    adopt(stream)
}

fn adopt(stream: UnixStream) -> Result<(), String> {
    stream
        .set_write_timeout(Some(CHANNEL_WRITE_TIMEOUT))
        .map_err(|_| "cannot bound execution channel writes")?;
    let reader = stream
        .try_clone()
        .map_err(|_| "cannot bind the protected execution channel")?;
    let closed = Arc::new(AtomicBool::new(false));
    let client = Arc::new(Client {
        writer: Mutex::new(stream),
        inflight: Mutex::new(std::collections::HashMap::new()),
        next_id: AtomicU64::new(1),
        closed: Arc::clone(&closed),
    });
    let pump = Arc::clone(&client);
    std::thread::Builder::new()
        .name("leanctx-exec-client".into())
        .spawn(move || pump.pump(reader))
        .map_err(|_| "cannot start the protected execution reader")?;
    CLIENT
        .set(client)
        .map_err(|_| "the protected execution channel is already bound".to_string())
}

impl Client {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Demultiplex supervisor events onto the waiting requests. Any protocol
    /// error ends the channel: every waiter then observes a disconnect and
    /// fails closed rather than silently losing a result.
    fn pump(&self, mut reader: UnixStream) {
        while let Ok(Some(frame)) = read_frame(&mut reader) {
            match serde_json::from_slice::<Envelope>(&frame) {
                Ok(envelope) if envelope.version == PROTOCOL_VERSION => {
                    let sender = self
                        .inflight
                        .lock()
                        .ok()
                        .and_then(|table| table.get(&envelope.id).cloned());
                    if let Some(sender) = sender {
                        let advisory = matches!(envelope.event, Event::Live { .. });
                        if matches!(
                            sender.try_send(envelope.event),
                            Err(mpsc::TrySendError::Full(_))
                        ) && !advisory
                        {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        self.closed.store(true, Ordering::Release);
        let _ = reader.shutdown(std::net::Shutdown::Both);
        if let Ok(mut table) = self.inflight.lock() {
            table.clear();
        }
    }

    fn begin(&self) -> Result<(u64, mpsc::Receiver<Event>), String> {
        if self.is_closed() {
            return Err("the protected execution channel closed".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut table = self
            .inflight
            .lock()
            .map_err(|_| "the protected execution channel is unusable")?;
        if self.is_closed() {
            return Err("the protected execution channel closed".into());
        }
        if table.len() >= MAX_INFLIGHT {
            return Err("too many concurrent supervised commands".into());
        }
        table.insert(id, sender);
        Ok((id, receiver))
    }

    fn finish(&self, id: u64) {
        if let Ok(mut table) = self.inflight.lock() {
            table.remove(&id);
        }
    }

    fn send(&self, id: u64, body: Body) -> Result<(), String> {
        let frame = serde_json::to_vec(&Request {
            version: PROTOCOL_VERSION,
            id,
            body,
        })
        .map_err(|_| "cannot encode a supervised request")?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "the protected execution channel is unusable")?;
        write_frame(&mut writer, &frame).map_err(|_| {
            self.closed.store(true, Ordering::Release);
            let _ = writer.shutdown(std::net::Shutdown::Both);
            "the protected execution channel closed".to_string()
        })
    }

    /// Run a shell command under the launcher's boundary and return exactly what
    /// the shared executor produced in the supervisor: its text and its real
    /// exit code, including the timeout and truncation notices.
    pub(crate) fn shell(
        &self,
        command: &str,
        cwd: &str,
        env: &std::collections::HashMap<String, String>,
        timeout_ms: Option<u64>,
        cancel: Option<&AtomicBool>,
        idle_keyed: bool,
        live: Option<&Mutex<String>>,
    ) -> (String, i32) {
        let body = Body::Shell {
            command: command.to_string(),
            cwd: cwd.to_string(),
            env: env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            timeout_ms,
            idle_keyed,
        };
        match self.request(body, cancel, live) {
            Ok(Event::ShellExit { text, code }) => (text, code),
            Ok(Event::Failed { message }) => (format!("ERROR: {message}"), 1),
            Ok(_) => ("ERROR: unexpected supervised execution response".into(), 1),
            Err(error) => (format!("ERROR: {error}"), 1),
        }
    }

    /// Run `ctx_execute`'s interpreter under its own narrower profile composed
    /// with the launch denials. The caller cannot request a weaker boundary.
    pub(crate) fn program(
        &self,
        read_paths: &[&std::path::Path],
        program: &str,
        args: &[&str],
        env: &[(String, String)],
        timeout_secs: u64,
        cwd: Option<&std::path::Path>,
    ) -> Result<(String, String, i32), String> {
        self.program_with_boundary(read_paths, program, args, env, timeout_secs, cwd, true)
    }

    /// Level-zero snippets still require the immutable session boundary.
    pub(crate) fn session_program(
        &self,
        program: &str,
        args: &[&str],
        env: &[(String, String)],
        timeout_secs: u64,
        cwd: Option<&std::path::Path>,
    ) -> Result<(String, String, i32), String> {
        self.program_with_boundary(&[], program, args, env, timeout_secs, cwd, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn program_with_boundary(
        &self,
        read_paths: &[&std::path::Path],
        program: &str,
        args: &[&str],
        env: &[(String, String)],
        timeout_secs: u64,
        cwd: Option<&std::path::Path>,
        constrained: bool,
    ) -> Result<(String, String, i32), String> {
        let body = Body::Program {
            constrained,
            read_paths: read_paths
                .iter()
                .map(|path| {
                    path.to_str()
                        .map(str::to_owned)
                        .ok_or("program read path is not UTF-8".to_string())
                })
                .collect::<Result<_, _>>()?,
            program: program.to_string(),
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: env.to_vec(),
            timeout_secs,
            cwd: cwd
                .map(|dir| {
                    dir.to_str()
                        .map(str::to_string)
                        .ok_or_else(|| "working directory is not valid UTF-8".to_string())
                })
                .transpose()?,
        };
        match self.request(body, None, None)? {
            Event::ProgramExit {
                stdout,
                stderr,
                code,
            } => Ok((stdout, stderr, code)),
            Event::Failed { message } => Err(message),
            _ => Err("unexpected supervised execution response".into()),
        }
    }

    fn request(
        &self,
        body: Body,
        cancel: Option<&AtomicBool>,
        live: Option<&Mutex<String>>,
    ) -> Result<Event, String> {
        let (id, events) = self.begin()?;
        let outcome = self.wait(id, body, cancel, live, &events);
        self.finish(id);
        outcome
    }

    fn wait(
        &self,
        id: u64,
        body: Body,
        cancel: Option<&AtomicBool>,
        live: Option<&Mutex<String>>,
        events: &mpsc::Receiver<Event>,
    ) -> Result<Event, String> {
        self.send(id, body)?;
        let mut cancelled = false;
        let deadline = Instant::now() + MAX_REQUEST_WALL + TEARDOWN_GRACE;
        loop {
            if Instant::now() >= deadline {
                let _ = self.send(id, Body::Cancel);
                return Err("supervised command exceeded its absolute lifetime".into());
            }
            match events.recv_timeout(CLIENT_TICK) {
                Ok(Event::Live { text }) => {
                    if let Some(live) = live
                        && let Ok(mut guard) = live.try_lock()
                    {
                        *guard = text;
                    }
                }
                Ok(event) => return Ok(event),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.is_closed() {
                        return Err("the protected execution channel closed".into());
                    }
                    if !cancelled
                        && cancel.is_some_and(|signal| signal.load(Ordering::Acquire))
                        && self.send(id, Body::Cancel).is_ok()
                    {
                        cancelled = true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("the protected execution channel closed".into());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor — the launcher wrapper's end
// ---------------------------------------------------------------------------

struct Supervisor {
    writer: Mutex<UnixStream>,
    profile: LaunchProfile,
    running: Mutex<std::collections::HashMap<u64, Arc<AtomicBool>>>,
}

/// Serve supervised execution for one protected MCP server until it
/// disconnects. Returns when the channel ends; in-flight children are then
/// cancelled and their process groups terminated.
pub(crate) fn serve(stream: UnixStream, profile: LaunchProfile) -> Result<(), String> {
    stream
        .set_write_timeout(Some(CHANNEL_WRITE_TIMEOUT))
        .map_err(|_| "cannot bound supervisor channel writes")?;
    let mut reader = stream
        .try_clone()
        .map_err(|_| "cannot bind the supervised execution channel")?;
    let supervisor = Arc::new(Supervisor {
        writer: Mutex::new(stream),
        profile,
        running: Mutex::new(std::collections::HashMap::new()),
    });
    let mut workers: Vec<std::thread::JoinHandle<()>> = Vec::new();
    // EOF, a truncated frame or malformed length closes the capability.
    while let Ok(Some(frame)) = read_frame(&mut reader) {
        let Ok(request): Result<Request, _> = serde_json::from_slice(&frame) else {
            supervisor.fail(0, "malformed supervised request");
            break;
        };
        if request.version != PROTOCOL_VERSION {
            supervisor.fail(request.id, "unsupported supervised protocol version");
            continue;
        }
        if matches!(request.body, Body::Cancel) {
            supervisor.cancel(request.id);
            continue;
        }
        workers.retain(|worker| !worker.is_finished());
        if workers.len() >= MAX_INFLIGHT {
            supervisor.fail(request.id, "too many concurrent supervised commands");
            continue;
        }
        let worker = Arc::clone(&supervisor);
        let id = request.id;
        let body = request.body;
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut running = supervisor
                .running
                .lock()
                .map_err(|_| "supervisor state unavailable")?;
            if running.contains_key(&id) {
                break;
            }
            running.insert(id, Arc::clone(&cancel));
        }
        if let Ok(handle) = std::thread::Builder::new()
            .name("leanctx-exec-worker".into())
            .spawn(move || worker.run(id, body, &cancel))
        {
            workers.push(handle);
        } else {
            if let Ok(mut running) = supervisor.running.lock() {
                running.remove(&id);
            }
            supervisor.fail(id, "cannot start a supervised command");
        }
    }
    let _ = reader.shutdown(std::net::Shutdown::Both);
    supervisor.shutdown();
    let deadline = Instant::now() + TEARDOWN_GRACE;
    for worker in workers {
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(CLIENT_TICK);
        }
        if !worker.is_finished() {
            break;
        }
        let _ = worker.join();
    }
    Ok(())
}

impl Supervisor {
    fn emit(&self, id: u64, event: Event) {
        let Ok(mut frame) = serde_json::to_vec(&Envelope {
            version: PROTOCOL_VERSION,
            id,
            event,
        }) else {
            return;
        };
        if frame.len() > MAX_FRAME_BYTES {
            frame = serde_json::to_vec(&Envelope {
                version: PROTOCOL_VERSION,
                id,
                event: Event::Failed {
                    message: "supervised output exceeds the channel limit".into(),
                },
            })
            .expect("fixed failure envelope");
        }
        if let Ok(mut writer) = self.writer.lock() {
            if write_frame(&mut writer, &frame).is_err() {
                let _ = writer.shutdown(std::net::Shutdown::Both);
                self.shutdown();
            }
        }
    }

    fn fail(&self, id: u64, message: &str) {
        self.emit(
            id,
            Event::Failed {
                message: message.to_string(),
            },
        );
    }

    fn cancel(&self, id: u64) {
        if let Ok(table) = self.running.lock()
            && let Some(signal) = table.get(&id)
        {
            signal.store(true, Ordering::Release);
        }
    }

    /// The MCP server is gone: cancel everything still running. The shared
    /// executor turns a set cancel flag into a full process-group kill.
    fn shutdown(&self) {
        if let Ok(table) = self.running.lock() {
            for signal in table.values() {
                signal.store(true, Ordering::Release);
            }
        }
    }

    fn run(self: Arc<Self>, id: u64, body: Body, cancel: &Arc<AtomicBool>) {
        // Absolute supervisor-side lifetime, independent of the executor's own
        // idle or wall budget.
        let watchdog = {
            let cancel = Arc::clone(cancel);
            let (done, finished) = mpsc::sync_channel(1);
            let handle = std::thread::Builder::new()
                .name("leanctx-exec-watchdog".into())
                .spawn(move || {
                    if matches!(
                        finished.recv_timeout(MAX_REQUEST_WALL),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        cancel.store(true, Ordering::Release);
                    }
                })
                .ok();
            (done, handle)
        };

        let event = if watchdog.1.is_none() {
            Event::Failed {
                message: "cannot bound supervised command lifetime".into(),
            }
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match body {
                Body::Shell {
                    command,
                    cwd,
                    env,
                    timeout_ms,
                    idle_keyed,
                } => self.run_shell(id, &command, &cwd, env, timeout_ms, idle_keyed, cancel),
                Body::Program {
                    constrained,
                    read_paths,
                    program,
                    args,
                    env,
                    timeout_secs,
                    cwd,
                } => self.run_program(
                    constrained,
                    &read_paths,
                    &program,
                    &args,
                    &env,
                    timeout_secs,
                    cwd,
                    cancel,
                ),
                Body::Cancel => Event::Failed {
                    message: "cancellation is not a command".into(),
                },
            }))
            .unwrap_or_else(|_| {
                cancel.store(true, Ordering::Release);
                Event::Failed {
                    message: "supervised command ended without a result".into(),
                }
            })
        };

        let _ = watchdog.0.send(());
        if let Some(handle) = watchdog.1 {
            let _ = handle.join();
        }
        if let Ok(mut table) = self.running.lock() {
            table.remove(&id);
        }
        self.emit(id, event);
    }

    #[allow(clippy::too_many_arguments)]
    fn run_shell(
        self: &Arc<Self>,
        id: u64,
        command: &str,
        cwd: &str,
        env: BTreeMap<String, String>,
        timeout_ms: Option<u64>,
        idle_keyed: bool,
        cancel: &Arc<AtomicBool>,
    ) -> Event {
        let extra: std::collections::HashMap<String, String> = env.into_iter().collect();
        let live = Arc::new(Mutex::new(String::new()));
        let (done, finished) = mpsc::sync_channel(1);
        let mirror = {
            let supervisor = Arc::clone(self);
            let live = Arc::clone(&live);
            std::thread::Builder::new()
                .name("leanctx-exec-live".into())
                .spawn(move || supervisor.mirror(id, &live, &finished))
                .ok()
        };
        // The one place the fixed profile enters: pinned by the launcher, taken
        // from this supervisor, never from the request.
        let profile = self.profile.shell.clone();
        let (text, code) = with_broker_profile(profile, || {
            crate::server::execute::execute_command_with_env_cancellable(
                command,
                cwd,
                &extra,
                timeout_ms.map(|ms| ms.min(MAX_REQUEST_WALL.as_millis() as u64)),
                Some(cancel.as_ref()),
                idle_keyed,
                Some(live.as_ref()),
            )
        });
        let _ = done.send(());
        if let Some(handle) = mirror {
            let _ = handle.join();
        }
        Event::ShellExit { text, code }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_program(
        &self,
        constrained: bool,
        read_paths: &[String],
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout_secs: u64,
        cwd: Option<String>,
        cancel: &Arc<AtomicBool>,
    ) -> Event {
        let mut composed = match self.profile.compose(read_paths, program) {
            Ok(composed) => composed,
            Err(message) => return Event::Failed { message },
        };
        if !constrained {
            // This matches the shell boundary, never an unprotected child.
            composed.clone_from(&self.profile.shell);
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let directory = cwd.map(std::path::PathBuf::from);
        match crate::core::sandbox_seatbelt::execute_with_profile(
            &composed,
            program,
            &borrowed,
            env,
            timeout_secs.min(MAX_REQUEST_WALL.as_secs()),
            directory.as_deref(),
            Some(cancel.as_ref()),
        ) {
            Ok((stdout, stderr, code)) => Event::ProgramExit {
                stdout,
                stderr,
                code,
            },
            Err(message) => Event::Failed { message },
        }
    }

    /// Mirror the captured-so-far output while the child runs, so a detached
    /// background job's `status` poll still shows progress across the channel.
    /// Best effort and bounded: snapshots are capped at the same ceiling the
    /// executor applies to the final capture.
    fn mirror(&self, id: u64, live: &Mutex<String>, done: &mpsc::Receiver<()>) {
        let cap = crate::core::limits::max_shell_bytes().min(2 * 1024 * 1024);
        let mut last = String::new();
        while matches!(
            done.recv_timeout(LIVE_INTERVAL),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            let Ok(guard) = live.lock() else { return };
            let snapshot = guard.clone();
            drop(guard);
            if snapshot == last {
                continue;
            }
            last.clone_from(&snapshot);
            let bounded = if snapshot.len() > cap {
                let mut start = snapshot.len() - cap;
                while start < snapshot.len() && !snapshot.is_char_boundary(start) {
                    start += 1;
                }
                snapshot[start..].to_string()
            } else {
                snapshot
            };
            self.emit(id, Event::Live { text: bounded });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().expect("socket pair")
    }

    #[test]
    fn frames_round_trip_and_reject_oversize_and_truncation() {
        let (mut read, mut write) = pair();
        write_frame(&mut write, b"payload").unwrap();
        drop(write);
        assert_eq!(read_frame(&mut read).unwrap().unwrap(), b"payload");
        assert!(read_frame(&mut read).unwrap().is_none());

        let (mut read, mut write) = pair();
        // A length header that promises more than the channel allows must be
        // refused before any allocation of that size.
        write
            .write_all(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())
            .unwrap();
        drop(write);
        assert!(read_frame(&mut read).is_err());

        let (mut read, mut write) = pair();
        write.write_all(&64_u32.to_be_bytes()).unwrap();
        write.write_all(b"short").unwrap();
        drop(write);
        assert!(read_frame(&mut read).unwrap().is_none());

        let (_read, mut write) = pair();
        assert!(write_frame(&mut write, &vec![0_u8; MAX_FRAME_BYTES + 1]).is_err());
    }

    #[test]
    fn a_program_profile_may_only_narrow() {
        let launch = LaunchProfile::synthetic("(deny file-read* (subpath \"/store\"))\n");
        let composed = launch
            .compose(&["/store".into()], "/bin/cat")
            .expect("composed");
        assert!(composed.starts_with("(version 1)\n(deny default)"));
        assert!(
            composed.find("(deny file-read*") > composed.find("(allow file-read*"),
            "the launch denials must come last: {composed}"
        );
        assert!(composed.ends_with(&launch.denials));
        assert!(launch.compose(&["relative".into()], "/bin/cat").is_err());
        assert!(launch.compose(&["/path\nrule".into()], "/bin/cat").is_err());
        assert!(
            launch
                .compose(&vec!["/store".into(); 65], "/bin/cat")
                .is_err()
        );
        assert!(launch.compose(&[], "cat").is_err());
        let quoted = launch
            .compose(&["/a\") (allow default) (\"b".into()], "/bin/cat")
            .unwrap();
        assert!(quoted.contains("/a\\\") (allow default) (\\\"b"));
        let legacy = serde_json::json!({"version":1,"id":1,"body":{"program":{
            "profile":"(version 1)\n(allow default)","read_paths":[],
            "program":"/bin/cat","args":[],"env":[],"timeout_secs":10,"cwd":null
        }}});
        assert!(serde_json::from_value::<Request>(legacy).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn combined_shell_retains_controls_and_refuses_new_grants() {
        let _lock = crate::core::data_dir::test_env_lock();
        let isolation = crate::core::data_dir::isolated_data_dir();
        let _session = crate::cli::pin_synthetic_session(isolation.path()).unwrap();
        let control = "(deny file-read* (literal \"/synthetic-control\"))\n";
        let combined =
            LaunchProfile::from_pinned(&format!("{LAUNCH_PROFILE_HEADER}{control}")).unwrap();
        assert!(combined.shell.ends_with(control));
        assert!(
            combined
                .compose(&["/".into()], "/bin/cat")
                .unwrap()
                .ends_with(control)
        );
        assert!(
            LaunchProfile::from_pinned(&format!("{LAUNCH_PROFILE_HEADER}(allow file-read*)\n"))
                .is_err()
        );
        assert!(
            LaunchProfile::from_pinned(&format!("{LAUNCH_PROFILE_HEADER}  {control}")).is_err()
        );
    }

    #[test]
    fn a_community_session_runs_children_directly() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        assert!(matches!(launch_mode().unwrap(), Mode::Direct));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_protected_session_without_a_channel_runs_nothing() {
        let _lock = crate::core::data_dir::test_env_lock();
        let isolation = crate::core::data_dir::isolated_data_dir();
        let _session = crate::cli::pin_synthetic_session(isolation.path()).expect("pinned rules");
        // No channel was installed in this process: the mode must be an error,
        // never a direct, unsupervised child.
        match launch_mode() {
            Err(message) => assert!(
                message.contains("supervised execution channel"),
                "{message}"
            ),
            Ok(_) => panic!("a protected session must not fall back to an unsupervised child"),
        }
    }

    #[test]
    fn a_supervisor_thread_takes_the_broker_private_path() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let profile = format!("{LAUNCH_PROFILE_HEADER}(deny file-read* (subpath \"/store\"))\n");
        with_broker_profile(profile.clone(), || match launch_mode().unwrap() {
            Mode::Sandboxed(fixed) => {
                assert_eq!(fixed, profile);
                assert_eq!(
                    sandbox_prefix(&fixed),
                    vec![SANDBOX_EXEC.to_string(), "-p".to_string(), profile.clone()]
                );
            }
            _ => panic!("a supervisor worker must apply the fixed profile itself"),
        });
        // The thread-local is scoped: it must not leak to the next request.
        assert!(broker_profile().is_none());
        with_broker_profile(profile, || {
            assert!(
                crate::core::sandbox_seatbelt::execute_sandboxed(
                    "/usr/bin/true",
                    &[],
                    &[],
                    &[],
                    1,
                    None
                )
                .is_err()
            );
        });
    }

    #[test]
    fn standard_streams_and_closed_descriptors_are_refused() {
        for fd in [-1, 0, 1, 2, i32::MAX] {
            assert!(install_client(fd).is_err());
        }
    }

    /// Supervisor and client in one process, over a real socket pair and real
    /// child processes — the topology the wrapper runs, minus the sandbox.
    #[cfg(target_os = "macos")]
    fn supervised<T>(denials: &str, exercise: impl FnOnce(&Client) -> T) -> T {
        let (supervisor_end, client_end) = pair();
        let profile = LaunchProfile::synthetic(denials);
        let server = std::thread::spawn(move || serve(supervisor_end, profile));
        let reader = client_end.try_clone().expect("clone");
        let closed = Arc::new(AtomicBool::new(false));
        let client = Arc::new(Client {
            writer: Mutex::new(client_end),
            inflight: Mutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: Arc::clone(&closed),
        });
        let pump = Arc::clone(&client);
        let reader_thread = std::thread::spawn(move || pump.pump(reader));
        let result = exercise(&client);
        // Close both cloned socket handles, as process exit does in production.
        client
            .writer
            .lock()
            .unwrap()
            .shutdown(std::net::Shutdown::Both)
            .unwrap();
        drop(client);
        let _ = server.join();
        let _ = reader_thread.join();
        result
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ordinary_work_succeeds_and_exit_codes_survive_the_channel() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let temp = tempfile::tempdir().expect("temp");
        std::fs::write(temp.path().join("ordinary.txt"), "project-content").unwrap();
        let cwd = temp.path().to_str().unwrap().to_string();
        supervised("", |client| {
            let (text, code) = client.shell(
                "cat ordinary.txt",
                &cwd,
                &std::collections::HashMap::new(),
                Some(30_000),
                None,
                false,
                None,
            );
            assert_eq!(code, 0, "{text}");
            assert!(text.contains("project-content"), "{text}");

            let (_, failing) = client.shell(
                "exit 17",
                &cwd,
                &std::collections::HashMap::new(),
                Some(30_000),
                None,
                false,
                None,
            );
            assert_eq!(failing, 17, "the child's real exit code must survive");
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_supervised_child_is_denied_the_store_but_keeps_its_project_work() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let temp = tempfile::tempdir().expect("temp");
        let store = temp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let store = store.canonicalize().unwrap();
        let secret = store.join("knowledge.json");
        std::fs::write(&secret, "synthetic-provider-fact").unwrap();
        let work = temp.path().join("project");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("ordinary.txt"), "project-content").unwrap();
        let denials = format!("(deny file-read* (subpath \"{}\"))\n", store.display());
        let cwd = work.to_str().unwrap().to_string();
        let secret_path = secret.to_str().unwrap().to_string();

        supervised(&denials, |client| {
            let (text, code) = client.shell(
                "cat ordinary.txt",
                &cwd,
                &std::collections::HashMap::new(),
                Some(30_000),
                None,
                false,
                None,
            );
            assert_eq!(code, 0, "ordinary project work must still run: {text}");

            let (denied, code) = client.shell(
                &format!("cat {secret_path}"),
                &cwd,
                &std::collections::HashMap::new(),
                Some(30_000),
                None,
                false,
                None,
            );
            assert_ne!(code, 0, "the store must be denied: {denied}");
            assert!(
                !denied.contains("synthetic-provider-fact"),
                "no store content may cross the channel"
            );
            let (_, stderr, code) = client
                .session_program("/usr/bin/true", &[], &[], 10, None)
                .unwrap();
            assert_eq!(code, 0, "{stderr}");
            let (text, stderr, code) = client
                .session_program("/bin/cat", &[&secret_path], &[], 10, None)
                .unwrap();
            assert_ne!(code, 0, "{text} {stderr}");
            assert!(!text.contains("synthetic-provider-fact"));
        });
        assert_eq!(
            std::fs::read_to_string(&secret).unwrap(),
            "synthetic-provider-fact"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_request_cannot_select_a_weaker_boundary() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().canonicalize().unwrap();
        let secret = root.join("knowledge.json");
        std::fs::write(&secret, "synthetic-provider-fact").unwrap();
        let denials = format!("(deny file-read* (subpath \"{}\"))\n", root.display());
        supervised(&denials, |client| {
            let (_, stderr, code) = client
                .program(&[&root], "/usr/bin/true", &[], &[], 10, None)
                .expect("ordinary program starts");
            assert_eq!(
                code, 0,
                "the profile must permit normal execution: {stderr}"
            );
            // Even an overly broad read root cannot re-open protected data.
            let (_, _, code) = client
                .program(
                    &[&root],
                    "/bin/cat",
                    &[secret.to_str().unwrap()],
                    &[],
                    10,
                    None,
                )
                .expect("the composed profile must compile and run");
            assert_ne!(code, 0, "the launch denials must survive a read root");
        });
    }

    #[test]
    fn a_closed_channel_fails_closed_instead_of_running_a_child() {
        let (supervisor_end, client_end) = pair();
        let reader = client_end.try_clone().expect("clone");
        let client = Arc::new(Client {
            writer: Mutex::new(client_end),
            inflight: Mutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: Arc::new(AtomicBool::new(false)),
        });
        let pump = Arc::clone(&client);
        let reader_thread = std::thread::spawn(move || pump.pump(reader));
        drop(supervisor_end);
        let (text, code) = client.shell(
            "echo unreachable",
            "/",
            &std::collections::HashMap::new(),
            Some(5_000),
            None,
            false,
            None,
        );
        assert_eq!(code, 1);
        assert!(text.contains("channel closed"), "{text}");
        let _ = reader_thread.join();
    }

    #[test]
    fn a_malformed_frame_ends_the_channel_rather_than_running_it() {
        let (supervisor_end, mut client_end) = pair();
        let profile = LaunchProfile::synthetic("");
        let server = std::thread::spawn(move || serve(supervisor_end, profile));
        let payload = b"{\"not\":\"a request\"}";
        write_frame(&mut client_end, payload).unwrap();
        // The supervisor answers with a fixed refusal and stops serving.
        let frame = read_frame(&mut client_end).unwrap().expect("refusal");
        let envelope: Envelope = serde_json::from_slice(&frame).unwrap();
        assert!(matches!(envelope.event, Event::Failed { .. }));
        assert!(read_frame(&mut client_end).unwrap().is_none());
        drop(client_end);
        server.join().unwrap().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn program_output_larger_than_a_pipe_completes_and_is_bounded() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let file = root.join("output.txt");
        let content = "q".repeat(512 * 1024);
        std::fs::write(&file, &content).unwrap();
        supervised("", |client| {
            let (stdout, stderr, code) = client
                .program(
                    &[&root],
                    "/bin/cat",
                    &[file.to_str().unwrap()],
                    &[],
                    10,
                    None,
                )
                .unwrap();
            assert_eq!(code, 0, "{stderr}");
            assert_eq!(stdout, content);
            std::fs::write(&file, "q".repeat(2 * 1024 * 1024)).unwrap();
            let failure = client
                .program(
                    &[&root],
                    "/bin/cat",
                    &[file.to_str().unwrap()],
                    &[],
                    10,
                    None,
                )
                .unwrap_err();
            assert!(failure.contains("capture bound"), "{failure}");
            let (stdout, stderr, code) = client
                .program(
                    &[],
                    "/bin/sh",
                    &["-c", "printf partial; while :; do :; done"],
                    &[],
                    1,
                    None,
                )
                .unwrap();
            assert_ne!(code, 0);
            assert!(stdout.contains("partial"), "{stdout}");
            assert!(stderr.contains("timed out"), "{stderr}");
        });
    }

    #[test]
    fn oversized_final_output_returns_a_failure_frame() {
        let (supervisor_end, mut client_end) = pair();
        let supervisor = Supervisor {
            writer: Mutex::new(supervisor_end),
            profile: LaunchProfile::synthetic(""),
            running: Mutex::new(std::collections::HashMap::new()),
        };
        supervisor.emit(
            17,
            Event::ShellExit {
                text: "x".repeat(MAX_FRAME_BYTES),
                code: 0,
            },
        );
        let frame = read_frame(&mut client_end).unwrap().unwrap();
        let event: Envelope = serde_json::from_slice(&frame).unwrap();
        assert_eq!(event.id, 17);
        assert!(matches!(event.event, Event::Failed { .. }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cancellation_terminates_the_whole_process_group() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::cli::unpin_synthetic_session();
        let cancel = AtomicBool::new(false);
        supervised("", |client| {
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    std::thread::sleep(Duration::from_millis(400));
                    cancel.store(true, Ordering::Release);
                });
                let started = Instant::now();
                let (_, code) = client.shell(
                    "sleep 120 & sleep 120",
                    "/tmp",
                    &std::collections::HashMap::new(),
                    Some(120_000),
                    Some(&cancel),
                    false,
                    None,
                );
                assert_eq!(code, 130, "a cancelled command reports cancellation");
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "cancellation must not wait for the command's own timeout"
                );
            });
        });
    }
}
