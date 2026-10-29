// SPDX-License-Identifier: Apache-2.0
//! The Engine's side of the spawn helper (see `spawn_helper`): one helper per
//! Engine process, started on the first MCP shell call, and the handle on a
//! shell that the helper started.
//!
//! Failure rule: a helper that cannot be started, or that has died, is
//! started once more for the call at hand. If that fails too, the caller runs
//! the shell itself, as it did before helpers existed, and is handed a fixed
//! notice for the result. A call is never refused for want of a helper, and
//! never runs without one silently.

use std::collections::HashMap;
use std::io::PipeReader;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use super::spawn_helper::{HELLO, exit_status};
use super::wire::{Inbox, Reply, Request, SpawnSpec, Unsent, send_frame};

/// How long a started helper may take to say hello, and a running one to
/// answer a spawn request.
const ANSWER_BUDGET: Duration = Duration::from_secs(5);

/// The helper of this process, once one was needed.
static LINK: Mutex<Option<Arc<Link>>> = Mutex::new(None);

/// The connection to one helper process.
struct Link {
    socket: UnixStream,
    /// One frame at a time.
    sending: Mutex<()>,
    /// Where the reader hands each call's replies.
    calls: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    next_call: AtomicU64,
    /// The helper is gone, or is not to be used any more.
    gone: AtomicBool,
    helper: Mutex<Child>,
}

/// The command that starts a helper.
#[cfg(not(test))]
fn helper_command() -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(super::HELPER_MODE);
    Ok(command)
}

/// In the unit tests the running binary is the test harness; its helper test
/// runs the same helper function.
#[cfg(test)]
fn helper_command() -> std::io::Result<Command> {
    if HELPER_BROKEN.load(Ordering::Relaxed) {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            super::tests::HELPER_TEST,
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(super::tests::HELPER_ENV, "spawn-helper")
        .env_remove(super::tests::FAULT_ENV);
    if let Some(fault) = *HELPER_FAULT.lock().unwrap_or_else(PoisonError::into_inner) {
        command.env(super::tests::FAULT_ENV, fault);
    }
    Ok(command)
}

impl Link {
    /// Starts a helper and waits for its hello.
    fn start() -> Option<Arc<Self>> {
        let (ours, theirs) = UnixStream::pair().ok()?;
        super::wire::quiet_socket(ours.as_raw_fd());
        let mut command = helper_command().ok()?;
        command
            .stdin(OwnedFd::from(theirs))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let helper = command.spawn().ok()?;
        // The command keeps the helper's end of the socket; with it open the
        // helper's end of input could not be seen here.
        drop(command);
        let reading = ours.try_clone().ok();
        let link = Arc::new(Self {
            socket: ours,
            sending: Mutex::new(()),
            calls: Mutex::new(HashMap::new()),
            next_call: AtomicU64::new(1),
            gone: AtomicBool::new(false),
            helper: Mutex::new(helper),
        });
        let (hello, greeted) = mpsc::channel();
        let reader = reading.and_then(|reading| {
            let link = Arc::clone(&link);
            std::thread::Builder::new()
                .name("spawn-helper-replies".into())
                .spawn(move || link.read_replies(&reading, &hello))
                .ok()
        });
        if reader.is_none() || greeted.recv_timeout(ANSWER_BUDGET).is_err() {
            // Whatever was started is not a working helper: it is ended by
            // its handle. The reader, if there is one, reaps it.
            let mut helper = link.helper.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = helper.kill();
            if reader.is_none() {
                let _ = helper.wait();
            }
            drop(helper);
            link.abandon();
            return None;
        }
        Some(link)
    }

    /// The reader thread: routes replies to their calls until the connection
    /// ends, then reaps the helper.
    fn read_replies(&self, socket: &UnixStream, hello: &mpsc::Sender<()>) {
        let mut inbox = Inbox::default();
        let mut greeted = false;
        let mut understood = true;
        'connection: while matches!(inbox.fill(socket.as_raw_fd()), Ok(true)) {
            loop {
                let frame = match inbox.next_frame() {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(_) => {
                        understood = false;
                        break 'connection;
                    }
                };
                if !greeted {
                    if frame != HELLO {
                        understood = false;
                        break 'connection;
                    }
                    greeted = true;
                    let _ = hello.send(());
                    continue;
                }
                let Some(reply) = Reply::decode(&frame) else {
                    understood = false;
                    break 'connection;
                };
                let (Reply::Spawned { call, .. }
                | Reply::SpawnFailed { call, .. }
                | Reply::Exited { call, .. }) = reply;
                if let Some(sender) = self.lock_calls().get(&call) {
                    let _ = sender.send(reply);
                }
            }
        }
        self.gone.store(true, Ordering::SeqCst);
        // Every waiting call sees its channel close: the helper is gone.
        self.lock_calls().clear();
        let mut helper = self.helper.lock().unwrap_or_else(PoisonError::into_inner);
        if !understood {
            // Not a helper of this version: ended by its handle.
            let _ = helper.kill();
        }
        // A helper whose connection ended exits by itself, after it has
        // ended what it still held; it is only reaped here.
        let _ = helper.wait();
    }

    fn lock_calls(&self) -> std::sync::MutexGuard<'_, HashMap<u64, mpsc::Sender<Reply>>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stops using this helper. Closing the socket is its end of input.
    fn abandon(&self) {
        self.gone.store(true, Ordering::SeqCst);
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }

    fn send(&self, request: &Request, descriptors: &[std::os::fd::RawFd]) -> Result<(), Unsent> {
        let _one_frame = self.sending.lock().unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        if matches!(request, Request::Spawn { .. }) && SEND_FAILS_ONCE.swap(false, Ordering::SeqCst)
        {
            return Err(Unsent::Nothing);
        }
        send_frame(self.socket.as_raw_fd(), &request.encode(), descriptors)
    }
}

/// The live helper of this process; started, or started again, when there is
/// none.
fn link() -> Option<Arc<Link>> {
    let mut slot = LINK.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(link) = slot.as_ref()
        && !link.gone.load(Ordering::SeqCst)
    {
        return Some(Arc::clone(link));
    }
    *slot = Link::start();
    slot.clone()
}

/// The Engine ends in order: the helper exits and ends nothing. Without this
/// message the helper takes the closed socket for the Engine's death.
pub(crate) fn shutdown_spawn_helper() {
    let link = LINK.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(link) = link {
        // Unsent: the helper is gone already and signals nothing either way.
        let _ = link.send(&Request::Shutdown, &[]);
        link.gone.store(true, Ordering::SeqCst);
    }
}

/// What became of the request to start a shell through the helper.
pub(crate) enum HelperSpawn {
    /// The helper is the shell's parent.
    Helped {
        child: HelpedChild,
        stdout: PipeReader,
        stderr: PipeReader,
    },
    /// No helper, or a helper that could execute nothing; the command has
    /// provably not run: the caller starts the shell itself and shows the notice,
    /// if there is one, in front of the result.
    Direct(Option<&'static str>),
}

/// Starts the shell described by `command` — program, arguments, environment
/// and working directory; its input is null and its outputs are the returned
/// pipes — as a child of this process's spawn helper.
pub(crate) fn spawn_shell(command: &Command) -> HelperSpawn {
    if !super::GUARD_HOST.load(Ordering::Relaxed) {
        // Not the `lean-ctx` binary: nothing here can be started as a helper.
        // Same limit, and same single debug line, as `guard_child`.
        super::note_no_guard_host();
        return HelperSpawn::Direct(None);
    }
    let spec = SpawnSpec {
        program: command.get_program().to_os_string(),
        args: command
            .get_args()
            .map(std::ffi::OsStr::to_os_string)
            .collect(),
        env: effective_environment(command),
        cwd: command
            .get_current_dir()
            .map(|dir| dir.as_os_str().to_os_string()),
    };
    let mut request = Request::Spawn { call: 0, spec };
    // The helper at hand, and — if that one is proven not to have received
    // the request — one more. Then, or when a helper answered that it could
    // execute nothing, the caller starts the shell itself: the command has
    // provably not run yet (see `Link::spawn`).
    for _ in 0..2 {
        let Some(link) = link() else {
            continue;
        };
        match link.spawn(&mut request) {
            Attempt::Done(spawned) => return spawned,
            Attempt::NotStarted => {}
            Attempt::Refused => break,
        }
    }
    HelperSpawn::Direct(Some(super::HELPER_NOTICE))
}

/// The environment the shell gets: this process's, with the command's own
/// changes applied — what a direct spawn of `command` would pass on.
fn effective_environment(command: &Command) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => environment.insert(name.to_os_string(), value.to_os_string()),
            None => environment.remove(name),
        };
    }
    environment.into_iter().collect()
}

enum Attempt {
    Done(HelperSpawn),
    /// Proven: the request did not reach a helper. Nothing runs; another
    /// helper may be tried.
    NotStarted,
    /// Proven: the helper tried and executed nothing. Nothing runs; the same
    /// request would fail the same way with another helper.
    Refused,
}

impl Link {
    fn spawn(self: &Arc<Self>, request: &mut Request) -> Attempt {
        let (Ok((stdout, stdout_end)), Ok((stderr, stderr_end))) =
            (std::io::pipe(), std::io::pipe())
        else {
            return Attempt::NotStarted;
        };
        let call = self.next_call.fetch_add(1, Ordering::Relaxed);
        if let Request::Spawn { call: slot, .. } = request {
            *slot = call;
        }
        let (sender, replies) = mpsc::channel();
        self.lock_calls().insert(call, sender);
        let mut child = HelpedChild {
            call,
            pid: 0,
            link: Arc::clone(self),
            replies,
            status: None,
            settled: false,
            lost: false,
            unclear: false,
        };
        // THE DECISION whether a command may be started a second time.
        //
        // The helper starts the shell before it answers, and a command can
        // do all it does and end without writing a byte. So once a request
        // may have reached the helper, nothing seen from here — no answer,
        // empty pipes at their end, a dead helper — proves that the command
        // did not run. Exactly three things are proof that it did NOT:
        //
        // 1. the helper was already known to be gone before anything was
        //    sent (`gone`: its end of the socket had closed);
        // 2. the send failed before one byte of the frame left this process
        //    (`Unsent::Nothing`) — a partial send is not proof;
        // 3. the helper answered `SpawnFailed`: its `spawn` returned an
        //    error, which it does only when no program was executed.
        //
        // Everything else is an unclear outcome: the command is not started
        // again, here or by the caller, and the call says so.
        let sent = if self.gone.load(Ordering::SeqCst) {
            Err(Unsent::Nothing)
        } else {
            self.send(request, &[stdout_end.as_raw_fd(), stderr_end.as_raw_fd()])
        };
        let reply = match sent {
            Ok(()) => child.replies.recv_timeout(ANSWER_BUDGET).ok(),
            Err(Unsent::Nothing) => {
                child.settled = true;
                self.abandon();
                return Attempt::NotStarted;
            }
            Err(Unsent::Part) => None,
        };
        // The shell has its descriptors, or nobody will: ours are closed
        // either way, so the readers see the end of the shell's output.
        drop((stdout_end, stderr_end));
        match reply {
            Some(Reply::Spawned { pid, .. }) => child.pid = pid,
            Some(Reply::SpawnFailed { .. }) => {
                child.settled = true;
                return Attempt::Refused;
            }
            Some(Reply::Exited { .. }) | None => {
                // The helper died, hangs or got half a request. It is not
                // used again; the next call starts a new one.
                self.abandon();
                child.settled = true;
                child.unclear = true;
            }
        }
        Attempt::Done(HelperSpawn::Helped {
            child,
            stdout,
            stderr,
        })
    }
}

/// A shell whose parent is the spawn helper. What the Engine does with a
/// child process, it does with this through the helper: learn its exit, end
/// its group, let go of it.
pub(crate) struct HelpedChild {
    call: u64,
    pid: u32,
    link: Arc<Link>,
    replies: mpsc::Receiver<Reply>,
    status: Option<ExitStatus>,
    /// Nothing is left to ask of the helper for this call.
    settled: bool,
    /// The helper went away before it reported the shell's exit.
    lost: bool,
    /// The helper never said whether it started the shell.
    unclear: bool,
}

impl HelpedChild {
    /// The shell's process id, which is also the id of its session and group.
    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    /// The fixed notice for a call whose helper went away: before it said
    /// whether the command was started at all (it may or may not have run and
    /// was not repeated), or before it reported the shell's exit (the shell
    /// has no parent that answers for it any more). `None` otherwise.
    pub(crate) fn notice(&self) -> Option<&'static str> {
        if self.unclear {
            Some(super::UNCLEAR_NOTICE)
        } else {
            self.lost.then_some(super::END_NOTICE)
        }
    }

    /// Takes one answer of the helper; returns the exit status once known.
    fn note_reply(&mut self, reply: &Reply) -> Option<ExitStatus> {
        if let Reply::Exited { status, .. } = reply {
            self.status = Some(exit_status(*status));
        }
        self.status
    }

    /// The helper is gone and has not said how the shell ended.
    fn note_gone(&mut self) -> std::io::Error {
        self.lost = true;
        self.settled = true;
        std::io::Error::other("the spawn helper is gone")
    }

    /// The exit status, if the shell has exited. `Err`: nobody can say.
    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        if self.lost || self.unclear {
            return Err(self.note_gone());
        }
        match self.replies.try_recv() {
            Ok(reply) => Ok(self.note_reply(&reply)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(self.note_gone()),
        }
    }

    /// Ends the shell's whole group: the helper signals it while the leader
    /// is its unreaped child, and reaps the leader.
    pub(crate) fn kill(&mut self) {
        if !self.settled {
            self.settled = true;
            // Unsent: the helper is gone, nobody ends the group, and the
            // `wait` that follows reports the call as lost. Nothing is run
            // again on any outcome of this message.
            let _ = self.link.send(&Request::Kill { call: self.call }, &[]);
        }
    }

    /// Waits until the helper has reported the shell's exit, or is gone.
    pub(crate) fn wait(&mut self) {
        while self.status.is_none() && !self.lost {
            match self.replies.recv() {
                Ok(reply) => {
                    self.note_reply(&reply);
                }
                Err(_) => {
                    let _ = self.note_gone();
                }
            }
        }
    }

    /// Lets go of the call: the helper reaps the leader and never signals
    /// the group again. What the command left running stays.
    ///
    /// `false`: the release did not leave this process. The helper's end of
    /// the socket is then closed — the helper is gone or going — and a helper
    /// that sees the socket closed ends what it still holds, which includes
    /// this call's group if it had not read a release for it. The caller
    /// says so in the call's result ([`super::RELEASE_NOTICE`]).
    pub(crate) fn release(mut self) -> bool {
        if self.settled {
            return true;
        }
        self.settled = true;
        #[cfg(test)]
        if RELEASE_FAILS_ONCE.swap(false, Ordering::SeqCst) {
            return false;
        }
        let sent = self
            .link
            .send(&Request::Release { call: self.call }, &[])
            .is_ok();
        if !sent {
            tracing::warn!(
                "spawn helper: a release could not be sent; what the command left running may be ended"
            );
        }
        sent
    }
}

/// A handle that goes away without a release — an unwinding owner — ends the
/// shell's group instead of leaving it to run unowned.
impl Drop for HelpedChild {
    fn drop(&mut self) {
        self.kill();
        self.link.lock_calls().remove(&self.call);
    }
}

#[cfg(test)]
static SEND_FAILS_ONCE: AtomicBool = AtomicBool::new(false);

/// Tests only: the next spawn request fails before one byte of it is sent.
#[cfg(test)]
pub(crate) fn fail_next_spawn_send_for_test() {
    SEND_FAILS_ONCE.store(true, Ordering::SeqCst);
}

#[cfg(test)]
static RELEASE_FAILS_ONCE: AtomicBool = AtomicBool::new(false);

/// Tests only: the next release is not sent, as with a closed socket.
#[cfg(test)]
pub(crate) fn fail_next_release_send_for_test() {
    RELEASE_FAILS_ONCE.store(true, Ordering::SeqCst);
}

#[cfg(test)]
static HELPER_FAULT: Mutex<Option<&'static str>> = Mutex::new(None);

/// Tests only: the fault every helper started from now on runs with (see
/// `spawn_helper::start_with_fault`); `None` for a sound helper.
#[cfg(test)]
pub(crate) fn set_helper_fault_for_test(fault: Option<&'static str>) {
    *HELPER_FAULT.lock().unwrap_or_else(PoisonError::into_inner) = fault;
}

#[cfg(test)]
static HELPER_BROKEN: AtomicBool = AtomicBool::new(false);

/// Tests only: while set, no helper can be started in this process.
#[cfg(test)]
pub(crate) fn break_helper_for_test(broken: bool) {
    HELPER_BROKEN.store(broken, Ordering::Relaxed);
}

/// Tests only: the process id of this process's live helper, if any.
#[cfg(test)]
pub(crate) fn helper_pid_for_test() -> Option<u32> {
    let slot = LINK.lock().unwrap_or_else(PoisonError::into_inner);
    let link = slot
        .as_ref()
        .filter(|link| !link.gone.load(Ordering::SeqCst))?;
    Some(
        link.helper
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .id(),
    )
}

/// Tests only: ends this process's helper by its handle, as a crash would,
/// and waits until its end was seen.
#[cfg(test)]
pub(crate) fn kill_helper_for_test() {
    let link = LINK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .expect("a helper runs");
    let _ = link
        .helper
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .kill();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !link.gone.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "the helper's end is seen"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
