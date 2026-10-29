// SPDX-License-Identifier: Apache-2.0
//! The session starter: the process an SDK starts in place of the Engine.
//!
//! ```text
//! SDK ── starts ──> starter            (whatever group/session the SDK chose)
//!                     │ spawns, process_group(0); the Engine inherits stdio
//!                     ▼
//!                   Engine             (leader of a NEW group E, the
//!                     └─ descendants    starter's child, unreaped until E
//!                                       is empty)
//! ```
//!
//! The starter relays nothing. It ends group E — SIGKILL, repeated until no
//! live member is left — when the Engine exits, when it receives SIGTERM,
//! SIGINT or SIGHUP, when its parent is gone (`--watch-parent`), or when its
//! control descriptor is at its end (`--control-fd`). It exits afterwards,
//! with the Engine's status. It never exits before E is empty, so "the leader
//! is gone and something is left" cannot be seen by the SDK.
//!
//! A SIGKILL of the starter itself is covered by a guard (see `guard`) on
//! group E: it ends E when the starter is gone. The Engine's program is
//! executed only once that guard is ready (see `gate`): the new process
//! waits, in its own group, until then, and exits without executing anything
//! if the starter is gone first. So there is no moment at which an Engine
//! runs and nothing would end it.
//!
//! That the Engine leads a group of its own is checked at the gate; that the
//! starter is not a member is checked before every group signal. If the
//! first cannot be shown, the Engine is never executed and the starter
//! reports failure.

use std::ffi::OsString;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::gate::Refusal;
use super::{END_BUDGET, EXIT_CONTAINMENT, EXIT_USAGE, GroupEnd, SESSION_ACK};
use crate::shell::exec::{exited_unreaped, reap_within};

/// How often the starter looks at its stop conditions.
const TICK: Duration = Duration::from_millis(50);
/// How long an exited or killed Engine may take to become reapable.
const REAP_BUDGET: Duration = Duration::from_secs(2);

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn request_stop(_signal: libc::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

pub(super) struct Options {
    /// Write [`SESSION_ACK`] to standard output before the Engine starts.
    pub(super) ack: bool,
    /// End the group when the process that started the starter is gone.
    pub(super) watch_parent: bool,
    /// A descriptor whose end of input means "end the group".
    pub(super) control_fd: Option<libc::c_int>,
    pub(super) program: OsString,
    pub(super) args: Vec<OsString>,
    /// Tests only: `false` builds the wrong topology on purpose.
    #[cfg(test)]
    pub(super) engine_in_own_group: bool,
}

#[cfg(test)]
fn engine_in_own_group(options: &Options) -> bool {
    options.engine_in_own_group
}

#[cfg(not(test))]
fn engine_in_own_group(_: &Options) -> bool {
    true
}

/// `__session-starter [--ack] [--watch-parent] [--control-fd <n>] -- <program> [args…]`.
pub(super) fn main_explicit(args: impl Iterator<Item = OsString>) -> i32 {
    parse(args).map_or(EXIT_USAGE, |options| run(&options))
}

/// The form an SDK asks for with [`super::SESSION_ENV`]: this same command
/// line becomes the Engine. `None` when this binary cannot name itself; the
/// caller then runs as a plain Engine and no acknowledgement is written.
pub(super) fn main_requested() -> Option<i32> {
    let program = std::env::current_exe().ok()?;
    Some(run(&Options {
        ack: true,
        watch_parent: true,
        control_fd: None,
        program: program.into_os_string(),
        args: std::env::args_os().skip(1).collect(),
        #[cfg(test)]
        engine_in_own_group: true,
    }))
}

pub(super) fn parse(mut args: impl Iterator<Item = OsString>) -> Option<Options> {
    let mut ack = false;
    let mut watch_parent = false;
    let mut control_fd = None;
    loop {
        let flag = args.next()?;
        if flag == "--" {
            break;
        } else if flag == "--ack" {
            ack = true;
        } else if flag == "--watch-parent" {
            watch_parent = true;
        } else if flag == "--control-fd" {
            let value = args.next()?;
            control_fd = Some(
                value
                    .to_str()?
                    .parse::<libc::c_int>()
                    .ok()
                    .filter(|fd| *fd > libc::STDERR_FILENO)?,
            );
        } else {
            return None;
        }
    }
    Some(Options {
        ack,
        watch_parent,
        control_fd,
        program: args.next()?,
        args: args.collect(),
        #[cfg(test)]
        engine_in_own_group: true,
    })
}

/// The control descriptor: open for as long as its writer lives.
struct Control(libc::c_int);

impl Control {
    /// Makes the descriptor non-blocking and keeps it from the Engine.
    fn open(fd: libc::c_int) -> Option<Self> {
        // SAFETY: integer-only descriptor operations on a descriptor this
        // process was handed for exactly this purpose.
        unsafe {
            let status = libc::fcntl(fd, libc::F_GETFL);
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if status < 0
                || flags < 0
                || libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) < 0
                || libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0
            {
                return None;
            }
        }
        Some(Self(fd))
    }

    /// Whether the input is at its end. Data is read and ignored; a
    /// descriptor that cannot be read counts as ended.
    fn closed(&self) -> bool {
        let mut buffer = [0_u8; 64];
        loop {
            // SAFETY: the pointer and length describe `buffer`.
            let read = unsafe { libc::read(self.0, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read == 0 {
                return true;
            }
            if read < 0 {
                return !matches!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                );
            }
        }
    }
}

fn report(message: &str) {
    let line = format!("leanctx-containment: {message}\n");
    let _ = super::write_fd(libc::STDERR_FILENO, line.as_bytes());
}

/// The starter itself. Returns its exit code.
pub(super) fn run(options: &Options) -> i32 {
    // SAFETY: getppid has no preconditions.
    let parent = unsafe { libc::getppid() };
    let control = match options.control_fd.map(Control::open) {
        Some(None) => return EXIT_USAGE,
        Some(control) => control,
        None => None,
    };
    STOP_REQUESTED.store(false, Ordering::SeqCst);
    super::set_termination_signals(
        request_stop as extern "C" fn(libc::c_int) as libc::sighandler_t,
    );
    if options.ack && !super::write_fd(libc::STDOUT_FILENO, SESSION_ACK) {
        return EXIT_CONTAINMENT;
    }

    let mut command = Command::new(&options.program);
    command.args(&options.args).env_remove(super::SESSION_ENV);
    if engine_in_own_group(options) {
        command.process_group(0);
    }
    // The Engine's program is executed only once the backstop is ready: a
    // starter that is killed at any moment leaves either nothing, or an
    // Engine whose group the backstop ends. On a refusal nothing ran, so
    // there is no group to signal and no child to end.
    let (mut engine, backstop) = match super::gate::start_behind_guard(&mut command) {
        Ok(started) => started,
        Err(Refusal::NotStarted(error)) => {
            report(&format!("the Engine could not be started: {error}"));
            return EXIT_CONTAINMENT;
        }
        Err(Refusal::NotLeader) => {
            report("the Engine's process group could not be verified; no group was signalled");
            return EXIT_CONTAINMENT;
        }
        Err(Refusal::NoGuard(error)) => {
            report(&format!("the session guard could not be attached: {error}"));
            return EXIT_CONTAINMENT;
        }
    };
    // The Engine leads this group (checked at the gate); that the starter is
    // no member of it is checked by `end_pinned_group` before any signal.
    let group = engine.id() as libc::pid_t;
    release_session_pipes();

    while !must_end(&mut engine, parent, options.watch_parent, control.as_ref()) {
        std::thread::sleep(TICK);
    }

    // The Engine — running, or exited and not reaped — pins the group number.
    let end = super::end_pinned_group(group, END_BUDGET);
    let _ = backstop.finish();
    let status = reap_within(&mut engine, REAP_BUDGET);
    match end {
        GroupEnd::Empty => {}
        GroupEnd::Unconfirmed => {
            report("the Engine's group was signalled; its end could not be observed");
        }
        GroupEnd::Survivors | GroupEnd::Refused => {
            report("the Engine's group could not be ended");
            return EXIT_CONTAINMENT;
        }
    }
    let Some(status) = status else {
        report("the Engine could not be reaped");
        return EXIT_CONTAINMENT;
    };
    mirror(status)
}

fn must_end(
    engine: &mut Child,
    parent: libc::pid_t,
    watch_parent: bool,
    control: Option<&Control>,
) -> bool {
    !matches!(exited_unreaped(engine), Ok(false))
        || STOP_REQUESTED.load(Ordering::SeqCst)
        // SAFETY: getppid has no preconditions.
        || (watch_parent && parent > 1 && unsafe { libc::getppid() } != parent)
        || control.is_some_and(Control::closed)
}

/// The starter keeps none of the session's input and output: the SDK sees
/// their end when the Engine's tree has closed them. Error output is kept for
/// the starter's own reports.
fn release_session_pipes() {
    use std::os::fd::AsRawFd as _;
    let Ok(null) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    else {
        return;
    };
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO] {
        // SAFETY: replaces our own standard descriptors by /dev/null.
        unsafe { libc::dup2(null.as_raw_fd(), fd) };
    }
}

/// Ends this process the way the Engine ended.
fn mirror(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    let Some(signal) = status.signal() else {
        return EXIT_CONTAINMENT;
    };
    // SAFETY: restores the default disposition and sends the signal to this
    // process only.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
    128 + signal
}
