// SPDX-License-Identifier: Apache-2.0
//! Starting a process that does not run before its guard is ready.
//!
//! A guard joins the group of a process that already exists. If that process
//! were already running its program, whoever started it could be killed
//! between the start and the guard's readiness and leave the program running
//! with nothing to end it. So the new process waits at a gate — after the
//! fork, in its own group, before the program is executed — until the guard
//! has pinned its group and reported ready. The gate is a pipe whose write
//! end only the starting process holds: opened by one byte, and at its end
//! when that process is gone, in which case the new process exits without
//! ever executing the program.

use std::io::Read as _;
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command};
use std::thread::ScopedJoinHandle;

use super::CallGuard;

/// Why nothing was started.
pub(crate) enum Refusal {
    /// The process could not be created or its program not executed.
    NotStarted(std::io::Error),
    /// The new process does not lead a group of its own. Nothing ran.
    NotLeader,
    /// No guard could be attached. Nothing ran.
    NoGuard(String),
}

/// Starts `command` — which must ask for a process group of its own — with a
/// guard on that group. The program is executed only after the guard is
/// ready; on every refusal it was never executed, so there is nothing to end.
///
/// `command` is left with the gate's `pre_exec` step and must not be spawned
/// a second time.
pub(crate) fn start_behind_guard(command: &mut Command) -> Result<(Child, CallGuard), Refusal> {
    let (gate, open) = std::io::pipe().map_err(Refusal::NotStarted)?;
    let (mut announced, announce) = std::io::pipe().map_err(Refusal::NotStarted)?;
    let (gate_fd, open_fd, announce_fd) =
        (gate.as_raw_fd(), open.as_raw_fd(), announce.as_raw_fd());
    // SAFETY: the closure runs in the forked child and calls only close,
    // getpid, write and read, all async-signal-safe.
    unsafe {
        command.pre_exec(move || wait_at_gate(gate_fd, open_fd, announce_fd));
    }
    // `spawn` returns only once the program is executed, which the gate
    // holds back: it runs on a thread of its own while this one sees to the
    // guard. The thread owns the ends the child inherits, so they are open
    // at the fork and closed here when `spawn` is done.
    std::thread::scope(|scope| {
        let spawning = scope.spawn(move || {
            let spawned = command.spawn();
            drop((gate, announce));
            spawned
        });
        let mut pid = [0_u8; size_of::<libc::pid_t>()];
        if announced.read_exact(&mut pid).is_err() {
            // No process came into being, or it ended before it said who it is.
            drop(open);
            return Err(Refusal::NotStarted(match finish(spawning) {
                Err(error) => error,
                Ok(_) => std::io::Error::other("the process did not wait for its guard"),
            }));
        }
        let pid = libc::pid_t::from_ne_bytes(pid);
        #[cfg(test)]
        pause_before_guard(pid);
        // SAFETY: reads the group of our own child, which waits at the gate.
        let leads = unsafe { libc::getpgid(pid) } == pid;
        let guard = if leads {
            attach_guard(pid).map_err(Refusal::NoGuard)
        } else {
            Err(Refusal::NotLeader)
        };
        let guard = match guard {
            Ok(guard) => guard,
            Err(refusal) => {
                // The gate ends unopened: the process exits, `spawn` reaps it.
                drop(open);
                let _ = finish(spawning);
                return Err(refusal);
            }
        };
        let opened = super::write_fd(open.as_raw_fd(), &[1]);
        drop(open);
        match finish(spawning) {
            Ok(child) if opened => Ok((child, guard)),
            // Dropping the guard lets go of it: it ends the group and is reaped.
            Ok(_) => Err(Refusal::NotStarted(std::io::Error::other(
                "the gate could not be opened",
            ))),
            Err(error) => Err(Refusal::NotStarted(error)),
        }
    })
}

fn finish(spawning: ScopedJoinHandle<'_, std::io::Result<Child>>) -> std::io::Result<Child> {
    spawning
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("the start did not finish")))
}

/// The guard of the process that waits at the gate and leads its own group.
#[cfg(not(test))]
fn attach_guard(process: libc::pid_t) -> Result<CallGuard, String> {
    CallGuard::attach_to(process, process)
}

/// As above; a test can make the attach fail the two ways it can fail.
#[cfg(test)]
fn attach_guard(process: libc::pid_t) -> Result<CallGuard, String> {
    let nowhere = super::tests::NO_SUCH_GROUP;
    match GUARD_FAULT.with(std::cell::Cell::get) {
        None => CallGuard::attach_to(process, process),
        Some(GuardFault::CannotJoin) => CallGuard::attach_to(nowhere, nowhere),
        Some(GuardFault::WrongGroup) => CallGuard::attach_to(process, nowhere),
    }
}

/// Tests only: how the next guards started on this thread fail.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardFault {
    /// The guard cannot be put into the group at all.
    CannotJoin,
    /// The guard is in the group but was told to guard another one: it never
    /// reports ready.
    WrongGroup,
}

#[cfg(test)]
thread_local! {
    static GUARD_FAULT: std::cell::Cell<Option<GuardFault>> = const { std::cell::Cell::new(None) };
    static LAST_GATED: std::cell::Cell<libc::pid_t> = const { std::cell::Cell::new(0) };
}

/// Tests only: every guard attached behind the gate on this thread fails as
/// `fault` says, until it is reset with `None`.
#[cfg(test)]
pub(crate) fn set_guard_fault_for_test(fault: Option<GuardFault>) {
    GUARD_FAULT.with(|current| current.set(fault));
}

/// Tests only: the process that last waited at a gate started on this thread.
#[cfg(test)]
pub(crate) fn last_gated_for_test() -> libc::pid_t {
    LAST_GATED.with(std::cell::Cell::get)
}

/// Tests only: notes the waiting process, and — when asked to — holds the
/// starting process between "the new process exists" and "attach its guard"
/// and says so on standard output, so that a test can end the starting
/// process exactly there.
#[cfg(test)]
fn pause_before_guard(process: libc::pid_t) {
    LAST_GATED.with(|last| last.set(process));
    if std::env::var_os(super::tests::PAUSE_ENV).is_some() {
        let _ = super::write_fd(
            libc::STDOUT_FILENO,
            format!("FORKED {process}\n").as_bytes(),
        );
        std::thread::sleep(std::time::Duration::from_mins(1));
    }
}

/// The child's side, between fork and exec: says who it is, then waits until
/// the gate is opened. An `Err` makes the child exit without executing.
fn wait_at_gate(gate: RawFd, open: RawFd, announce: RawFd) -> std::io::Result<()> {
    // SAFETY: integer-only calls on descriptors inherited through the fork.
    // The child's copy of the gate's write end is closed first: only the
    // starting process keeps the gate from ending.
    unsafe {
        libc::close(open);
        let pid = libc::getpid().to_ne_bytes();
        let told = libc::write(announce, pid.as_ptr().cast(), pid.len());
        libc::close(announce);
        if told != pid.len() as isize {
            return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
        }
        let mut byte = 0_u8;
        loop {
            match libc::read(gate, (&raw mut byte).cast(), 1) {
                1 => return Ok(()),
                0 => return Err(std::io::Error::from_raw_os_error(libc::ECANCELED)),
                _ => {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(error);
                    }
                }
            }
        }
    }
}
