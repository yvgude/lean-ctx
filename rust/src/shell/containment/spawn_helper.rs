// SPDX-License-Identifier: Apache-2.0
//! The spawn helper: one long-lived process per Engine that is the PARENT of
//! the MCP shells (`server/execute.rs`).
//!
//! ```text
//! Engine ── socket (the helper's standard input) ──> helper   (a session of its own)
//!   │  spawn request + the shell's output descriptors            │ spawns; the shell calls
//!   │  kill / release / shutdown                                 │ setsid() before exec
//!   │                                                            ▼
//!   └────────── reads the shell's output directly ────────── shell (leader of its own
//!                                                              session and group, the
//!                                                              helper's child)
//! ```
//!
//! An MCP shell leads a session of its own, so nothing outside that session
//! can join its group and pin it. Its parent can: an unreaped child pins the
//! number of the group it leads. The helper is that parent, and it is outside
//! every shell's session.
//!
//! The helper decides nothing. Command line, environment, working directory
//! and descriptors are handed to it; allowlist, sandbox rules, timeouts and
//! output stay in the Engine. What it adds is the end of the story: when the
//! Engine's connection ends without a shutdown message — the Engine was
//! killed, crashed, or exited without saying so — it ends every group it
//! still holds, then exits. A group is held from the shell's start until the
//! Engine releases the call (a released call's background processes are never
//! signalled) or asks for its end (signalled while the leader is unreaped,
//! then reaped; never by number after the reap).
//!
//! It keeps none of the shells' descriptors: they are moved into the spawned
//! process. Its own output and error output are null.

use std::ffi::OsString;
use std::os::fd::{OwnedFd, RawFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

use super::wire::{Inbox, Reply, Request, SpawnSpec, send_frame};
use super::{END_BUDGET, EXIT_CONTAINMENT, EXIT_USAGE};
use crate::shell::exec::exited_unreaped;

/// The first frame a helper sends. An Engine that reads anything else is not
/// talking to a helper of its own version and does not use it.
pub(super) const HELLO: &[u8] = b"leanctx-spawn-helper 1";

/// The write end of the pipe that wakes the helper when a child changed.
static CHILD_EVENT: AtomicI32 = AtomicI32::new(-1);

extern "C" fn child_changed(_signal: libc::c_int) {
    let descriptor = CHILD_EVENT.load(Ordering::Relaxed);
    if descriptor >= 0 {
        // SAFETY: write is async-signal-safe; the pipe is non-blocking and is
        // drained on every round, so the write does not fail in practice.
        unsafe { libc::write(descriptor, [1_u8].as_ptr().cast(), 1) };
    }
}

/// Makes the shell the leader of a new session — and so of a new group, with
/// no controlling terminal. The one definition of how an MCP shell starts,
/// used by the helper and by the Engine's direct path.
pub(crate) fn lead_new_session(command: &mut Command) {
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// `__spawn-helper`, no arguments. Standard input is the Engine's socket.
pub(super) fn main(mut args: impl Iterator<Item = OsString>) -> i32 {
    if args.next().is_some() {
        return EXIT_USAGE;
    }
    serve(libc::STDIN_FILENO)
}

/// One shell the helper is the parent of.
struct Held {
    call: u64,
    child: Child,
    /// The Engine was told that the leader exited.
    reported: bool,
    /// The group was signalled on the Engine's request; reap when it exits.
    killed: bool,
    /// The Engine let go while the leader still ran: never signal, only reap.
    released: bool,
}

/// The helper itself. Returns its exit code.
pub(super) fn serve(socket: RawFd) -> i32 {
    // A session of its own: a signal to the Engine's group or terminal does
    // not remove the helper before it has seen the Engine's end. Fails only
    // for a group leader, which a spawned helper is not; then it stays.
    // SAFETY: plain syscall on this process.
    unsafe { libc::setsid() };
    let Some(wake) = watch_children() else {
        return EXIT_CONTAINMENT;
    };
    super::wire::quiet_socket(socket);
    if send_frame(socket, HELLO, &[]).is_err() {
        return 0;
    }
    let mut inbox = Inbox::default();
    let mut held: Vec<Held> = Vec::new();
    let orderly = loop {
        if !report_exits(socket, &mut held) {
            break false;
        }
        if !wait_for_event(socket, wake) {
            continue;
        }
        if !matches!(inbox.fill(socket), Ok(true)) {
            break false;
        }
        match handle_requests(socket, &mut inbox, &mut held) {
            Some(true) => break true,
            Some(false) => break false,
            None => {}
        }
    };
    if !orderly {
        end_held(&mut held);
    }
    0
}

/// Installs the child-change wake-up. Returns the read end of its pipe.
fn watch_children() -> Option<RawFd> {
    let mut ends: [RawFd; 2] = [0; 2];
    // SAFETY: integer-only descriptor calls on a pipe this process creates;
    // the handler only writes one byte to it.
    unsafe {
        if libc::pipe(ends.as_mut_ptr()) != 0 {
            return None;
        }
        for end in ends {
            libc::fcntl(end, libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(end, libc::F_SETFL, libc::O_NONBLOCK);
        }
        CHILD_EVENT.store(ends[1], Ordering::Relaxed);
        // Handlers, not changed dispositions that a shell would inherit:
        // handlers are reset by exec. The helper ends with its input, never
        // earlier: a signal meant for "every lean-ctx process" must not
        // remove it while the Engine may still be running — or dying.
        libc::signal(
            libc::SIGCHLD,
            child_changed as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            let before = libc::signal(
                signal,
                stay as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
            if before == libc::SIG_IGN {
                // Ignored by whoever started the Engine: a shell inherits
                // that from the Engine, and so it does from the helper.
                libc::signal(signal, libc::SIG_IGN);
            }
        }
    }
    Some(ends[0])
}

extern "C" fn stay(_signal: libc::c_int) {}

/// Blocks until the socket has input or a child changed. `true`: the socket
/// is to be read (input, end of input, or an error on it).
fn wait_for_event(socket: RawFd, wake: RawFd) -> bool {
    let mut waits = [
        libc::pollfd {
            fd: socket,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: the array is valid for its length.
    let ready = unsafe { libc::poll(waits.as_mut_ptr(), 2, -1) };
    if ready <= 0 {
        // Interrupted by the child signal: look at the children.
        return false;
    }
    if waits[1].revents != 0 {
        let mut drained = [0_u8; 64];
        // SAFETY: the pointer and length describe `drained`; the pipe is
        // non-blocking.
        while unsafe { libc::read(wake, drained.as_mut_ptr().cast(), drained.len()) } > 0 {}
    }
    waits[0].revents != 0
}

/// Handles every complete request. `Some(true)`: orderly shutdown.
/// `Some(false)`: the connection is of no use any more.
fn handle_requests(socket: RawFd, inbox: &mut Inbox, held: &mut Vec<Held>) -> Option<bool> {
    loop {
        let frame = match inbox.next_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => return None,
            Err(_) => return Some(false),
        };
        match Request::decode(&frame) {
            Some(Request::Shutdown) => return Some(true),
            Some(Request::Spawn { call, spec }) => {
                let (Some(stdout), Some(stderr)) =
                    (inbox.take_descriptor(), inbox.take_descriptor())
                else {
                    return Some(false);
                };
                #[cfg(test)]
                let started = start_with_fault(spec, stdout, stderr);
                #[cfg(not(test))]
                let started = start(spec, stdout, stderr);
                // An error here means that no program was executed: the
                // Engine may take `SpawnFailed` as proof of that.
                let reply = match started {
                    Ok(child) => {
                        let pid = child.id();
                        held.push(Held {
                            call,
                            child,
                            reported: false,
                            killed: false,
                            released: false,
                        });
                        Reply::Spawned { call, pid }
                    }
                    Err(error) => Reply::SpawnFailed {
                        call,
                        os_error: error.raw_os_error().unwrap_or(0),
                        message: error.to_string(),
                    },
                };
                if send_frame(socket, &reply.encode(), &[]).is_err() {
                    return Some(false);
                }
            }
            Some(Request::Kill { call }) => {
                if let Some(held) = held.iter_mut().find(|held| held.call == call) {
                    end_group_once(held);
                }
            }
            Some(Request::Release { call }) => {
                if let Some(held) = held.iter_mut().find(|held| held.call == call) {
                    held.released = true;
                }
            }
            None => return Some(false),
        }
    }
}

/// Starts one shell exactly as it was handed over. The descriptors are moved
/// into the command and closed here as soon as the shell has them.
fn start(spec: SpawnSpec, stdout: OwnedFd, stderr: OwnedFd) -> std::io::Result<Child> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .env_clear()
        .envs(spec.env)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    lead_new_session(&mut command);
    command.spawn()
}

/// Tests only: `start`, with the fault this helper process was started with.
/// `refuse`: answers a failed start without starting anything.
/// `die-after-start`: starts the shell, gives it time to do what it does, and
/// dies before the Engine is told.
#[cfg(test)]
fn start_with_fault(spec: SpawnSpec, stdout: OwnedFd, stderr: OwnedFd) -> std::io::Result<Child> {
    match std::env::var(super::tests::FAULT_ENV).as_deref() {
        Ok("refuse") => Err(std::io::Error::other("refused for a test")),
        Ok("die-after-start") => {
            let _started = start(spec, stdout, stderr)?;
            std::thread::sleep(std::time::Duration::from_millis(700));
            // SAFETY: ends this helper process at once, as a kill would.
            unsafe { libc::_exit(9) }
        }
        _ => start(spec, stdout, stderr),
    }
}

/// One SIGKILL to the group the held shell leads. Only ever called while the
/// shell is this process's unreaped child, which pins the group's number.
fn end_group_once(held: &mut Held) {
    if held.killed || held.released {
        return;
    }
    held.killed = true;
    let group = held.child.id() as libc::pid_t;
    #[cfg(test)]
    GROUP_SIGNAL_LOG.with(|log| {
        log.borrow_mut()
            .push(exited_unreaped(&mut held.child).is_ok());
    });
    // SAFETY: plain syscall; the group id is pinned by our unreaped child
    // and this process is in another session.
    unsafe { libc::killpg(group, libc::SIGKILL) };
}

/// Tells the Engine about every leader that has exited, and reaps those that
/// are no longer held. `false`: the Engine cannot be reached.
fn report_exits(socket: RawFd, held: &mut Vec<Held>) -> bool {
    let mut reachable = true;
    held.retain_mut(|held| {
        if !matches!(exited_unreaped(&mut held.child), Ok(true)) {
            return true;
        }
        if !held.reported {
            held.reported = true;
            let reply = Reply::Exited {
                call: held.call,
                status: unreaped_status(&held.child),
            };
            reachable &= send_frame(socket, &reply.encode(), &[]).is_ok();
        }
        // An exited leader stays unreaped — and its group held — until the
        // Engine says what becomes of the call.
        if held.killed || held.released {
            let _ = held.child.wait();
            return false;
        }
        true
    });
    reachable
}

/// The raw wait status of an exited child, read without reaping it.
fn unreaped_status(child: &Child) -> i32 {
    // SAFETY: an all-zero siginfo_t is a valid out-parameter for waitid.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: waitid on our own unreaped child; WNOWAIT leaves it waitable.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id() as libc::id_t,
            &raw mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return 1 << 8;
    }
    // SAFETY: waitid succeeded for an exited child; the status is set.
    let status = unsafe { info.si_status() };
    match info.si_code {
        libc::CLD_EXITED => (status & 0xff) << 8,
        libc::CLD_DUMPED => (status & 0x7f) | 0x80,
        _ => status & 0x7f,
    }
}

/// The Engine is gone without a word: every group still held is ended —
/// repeatedly, until no live member is left — and its leader reaped.
fn end_held(held: &mut Vec<Held>) {
    for mut held in held.drain(..) {
        if held.released {
            continue;
        }
        let group = held.child.id() as libc::pid_t;
        let _ = super::end_pinned_group(group, END_BUDGET);
        let _ = held.child.wait();
    }
}

/// What the Engine makes of a raw wait status.
pub(super) fn exit_status(raw: i32) -> std::process::ExitStatus {
    std::process::ExitStatus::from_raw(raw)
}

#[cfg(test)]
thread_local! {
    /// For each group signal of [`end_group_once`]: whether the leader was
    /// this process's unreaped child at that moment.
    static GROUP_SIGNAL_LOG: std::cell::RefCell<Vec<bool>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Tests only: runs one shell through the helper's own start, kill and reap
/// steps in this process and returns the signal log and the group.
#[cfg(test)]
pub(super) fn kill_path_for_test(spec: SpawnSpec) -> (Vec<bool>, libc::pid_t, bool) {
    let (_reader, writer) = std::io::pipe().expect("pipe");
    let stderr = writer.try_clone().expect("clone");
    let child = start(spec, writer.into(), stderr.into()).expect("the shell starts");
    let group = child.id() as libc::pid_t;
    let mut held = vec![Held {
        call: 1,
        child,
        reported: true,
        killed: false,
        released: false,
    }];
    std::thread::sleep(std::time::Duration::from_millis(200));
    end_group_once(&mut held[0]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !held.is_empty() && std::time::Instant::now() < deadline {
        report_exits(-1, &mut held);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // A second request for the same call finds nothing to signal.
    let reaped = held.is_empty();
    let log = GROUP_SIGNAL_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()));
    (log, group, reaped)
}
