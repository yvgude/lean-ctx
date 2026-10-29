// SPDX-License-Identifier: Apache-2.0
//! The guard of one process group, and the Engine's handle on it.
//!
//! The guard is started as a MEMBER of the group it guards (the spawner puts
//! it there before it runs). It then
//!
//! 1. checks that it is in the expected group,
//! 2. forks a pin — a child that exits at once and is not reaped, so the
//!    group's number stays allocated for as long as the guard lives,
//! 3. moves to a group of its own and checks that it left,
//! 4. reports that it is ready, and blocks until its input ends,
//! 5. ends the guarded group — from outside, so it survives the signal and
//!    can repeat it until no live member is left — and exits.
//!
//! It holds none of the guarded processes' pipes: its input is the control
//! pipe, its output carries the one ready line, its error output is null.

use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

use super::{END_BUDGET, EXIT_CONTAINMENT, EXIT_USAGE, GroupEnd};

/// The line a guard writes once the group is pinned and it has left it.
const READY: &[u8] = b"leanctx-group-guard ready\n";
/// Exit code: the signals were sent, the process table could not be read.
const EXIT_UNCONFIRMED: i32 = 2;
/// Exit code: live members remained when the budget was spent.
const EXIT_SURVIVORS: i32 = 3;
/// How long a started guard may take to report that it is ready.
const READY_BUDGET: Duration = Duration::from_secs(5);
/// How long an ended guard may take to become reapable.
const REAP_BUDGET: Duration = Duration::from_secs(2);

/// `__group-guard --group <pgid>`.
pub(super) fn main(mut args: impl Iterator<Item = std::ffi::OsString>) -> i32 {
    let group = match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(value), None) if flag == "--group" => value
            .to_str()
            .and_then(|value| value.parse::<libc::pid_t>().ok()),
        _ => None,
    };
    group.filter(|group| *group > 1).map_or(EXIT_USAGE, run)
}

/// The guard itself. Returns its exit code.
pub(super) fn run(expected: libc::pid_t) -> i32 {
    // SAFETY: getpgrp has no preconditions.
    if unsafe { libc::getpgrp() } != expected {
        return EXIT_CONTAINMENT;
    }
    // The guard ends with its input, never earlier: a signal meant for
    // "everything" must not remove the backstop while its owner still runs.
    super::set_termination_signals(libc::SIG_IGN);
    // SAFETY: the child calls nothing but _exit, which is async-signal-safe.
    let pin = unsafe { libc::fork() };
    if pin == 0 {
        // SAFETY: see above.
        unsafe { libc::_exit(0) };
    }
    if pin < 0 {
        return EXIT_CONTAINMENT;
    }
    // SAFETY: plain syscalls on this process. A process that is no session
    // leader may always open a group of its own.
    let left = unsafe { libc::setpgid(0, 0) } == 0 && unsafe { libc::getpgrp() } != expected;
    if !left {
        reap_pin(pin);
        return EXIT_CONTAINMENT;
    }
    // A failed write means the owner is gone already; the input ends at once.
    let _ = super::write_fd(libc::STDOUT_FILENO, READY);
    wait_for_end_of_input();
    let end = super::end_pinned_group(expected, END_BUDGET);
    reap_pin(pin);
    match end {
        GroupEnd::Empty => 0,
        GroupEnd::Unconfirmed => EXIT_UNCONFIRMED,
        GroupEnd::Survivors => EXIT_SURVIVORS,
        GroupEnd::Refused => EXIT_CONTAINMENT,
    }
}

/// Blocks until standard input is at its end. Data on it is ignored; an input
/// that cannot be read counts as ended, so a guard never waits on nothing.
fn wait_for_end_of_input() {
    let mut buffer = [0_u8; 64];
    loop {
        // SAFETY: the pointer and length describe `buffer`.
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, buffer.as_mut_ptr().cast(), buffer.len()) };
        if read == 0 {
            return;
        }
        if read < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

fn reap_pin(pin: libc::pid_t) {
    let mut status: libc::c_int = 0;
    // SAFETY: waits for our own child; the status pointer is valid.
    unsafe { libc::waitpid(pin, &raw mut status, 0) };
}

/// The command that starts a guard for `expected`.
#[cfg(not(test))]
fn guard_command(expected: libc::pid_t) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args([super::GUARD_MODE, "--group", &expected.to_string()]);
    Ok(command)
}

/// In the unit tests the running binary is the test harness; its helper test
/// runs the same guard function.
#[cfg(test)]
fn guard_command(expected: libc::pid_t) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            super::tests::HELPER_TEST,
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(super::tests::HELPER_ENV, "group-guard")
        .env(super::tests::GROUP_ENV, expected.to_string());
    Ok(command)
}

/// What a guard reported when its owner let go of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardOutcome {
    /// The group has no live member left.
    Ended,
    /// The group was signalled; whether it is empty could not be observed.
    Unconfirmed,
    /// The guard did not end the group, or did not finish in time.
    Failed,
}

/// The owner's handle on a guard: the guard ends its group when this handle
/// goes away — by [`CallGuard::finish`], by a drop, or by the owner's death.
pub(crate) struct CallGuard {
    guard: Child,
    hold: Option<std::io::PipeWriter>,
}

impl CallGuard {
    /// Starts a guard for the group `expected`, as a member of the group
    /// `join`; the two differ only in tests. The one caller is the gate
    /// (`gate::start_behind_guard`): the group's leader waits there and has
    /// not executed its program, so it cannot have exited.
    pub(super) fn attach_to(join: libc::pid_t, expected: libc::pid_t) -> Result<Self, String> {
        let (reader, writer) = std::io::pipe().map_err(|error| error.to_string())?;
        let mut command = guard_command(expected).map_err(|error| error.to_string())?;
        command
            .stdin(reader)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(join);
        let mut guard = command
            .spawn()
            .map_err(|error| format!("the guard could not join the group: {error}"))?;
        // The command keeps a copy of the read end; it is of no use any more.
        drop(command);
        let ready = guard
            .stdout
            .take()
            .is_some_and(|output| ready_within(output, READY_BUDGET));
        if !ready {
            // Whatever was started is not a working guard. Closing its input
            // lets a real one end; then it is ended by handle and reaped.
            drop(writer);
            let _ = guard.kill();
            let _ = crate::shell::exec::reap_within(&mut guard, REAP_BUDGET);
            return Err("the guard did not become ready".to_string());
        }
        Ok(Self {
            guard,
            hold: Some(writer),
        })
    }

    /// Lets go: the guard ends what is left of the group, and is reaped.
    pub(crate) fn finish(mut self) -> GuardOutcome {
        drop(self.hold.take());
        let outcome = if let Some(status) =
            crate::shell::exec::reap_within(&mut self.guard, END_BUDGET + REAP_BUDGET)
        {
            match status.code() {
                Some(0) => GuardOutcome::Ended,
                Some(EXIT_UNCONFIRMED) => GuardOutcome::Unconfirmed,
                _ => GuardOutcome::Failed,
            }
        } else {
            let _ = self.guard.kill();
            let _ = crate::shell::exec::reap_within(&mut self.guard, REAP_BUDGET);
            GuardOutcome::Failed
        };
        // Test seam: the guard has done its real work; only the reported
        // outcome is replaced, for the calling thread.
        #[cfg(test)]
        if let Some(forced) = FORCED_OUTCOME.with(std::cell::Cell::get) {
            return forced;
        }
        outcome
    }
}

/// A handle that goes away without [`CallGuard::finish`] — an early return,
/// an unwinding owner — still lets go of the guard and reaps it, so the group
/// is ended and no guard process is left behind as a zombie.
impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.hold.take().is_some()
            && crate::shell::exec::reap_within(&mut self.guard, END_BUDGET + REAP_BUDGET).is_none()
        {
            let _ = self.guard.kill();
            let _ = crate::shell::exec::reap_within(&mut self.guard, REAP_BUDGET);
        }
    }
}

#[cfg(test)]
thread_local! {
    static FORCED_OUTCOME: std::cell::Cell<Option<GuardOutcome>> =
        const { std::cell::Cell::new(None) };
}

/// Tests only: every `finish` on this thread reports `outcome` until it is
/// reset with `None`.
#[cfg(test)]
pub(crate) fn force_outcome_for_test(outcome: Option<GuardOutcome>) {
    FORCED_OUTCOME.with(|forced| forced.set(outcome));
}

/// Whether the ready line arrives within `budget`.
fn ready_within(mut output: ChildStdout, budget: Duration) -> bool {
    const MAX_PREAMBLE: usize = 8192;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut chunk = [0_u8; 256];
        let found = loop {
            match output.read(&mut chunk) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Ok(0) | Err(_) => break false,
                Ok(read) => {
                    seen.extend_from_slice(&chunk[..read]);
                    if seen.windows(READY.len()).any(|window| window == READY) {
                        break true;
                    }
                    if seen.len() > MAX_PREAMBLE {
                        break false;
                    }
                }
            }
        };
        let _ = sender.send(found);
    });
    receiver.recv_timeout(budget).unwrap_or(false)
}
