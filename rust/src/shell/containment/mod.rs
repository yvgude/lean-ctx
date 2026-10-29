// SPDX-License-Identifier: Apache-2.0
//! Process containment for agent-tool calls and SDK sessions (Unix).
//!
//! Two rules hold for every signal sent from this module:
//!
//! 1. A process group is signalled by its number only while that number is
//!    pinned by an unreaped child of the signalling process that belongs to
//!    the group (POSIX: a process id is not reused while a process group with
//!    that id exists, and a group exists until its last member — a zombie
//!    included — is gone).
//! 2. The signalling process has verified that it is not a member of the
//!    group. If that cannot be shown, nothing is signalled.
//!
//! The parts:
//!
//! - [`CallGuard`]: one guard process per agent-tool call. It joins the call's
//!   group, leaves a pin behind, moves to a group of its own and waits for its
//!   input to end — the call finished, or the Engine is gone. Then it ends the
//!   call's group, repeatedly, until no live member is left.
//! - the session starter (`starter`): the process an SDK starts. It runs the
//!   Engine in a new group, stays outside, and ends that group when the Engine
//!   exits, when it is asked to, or when its parent is gone. A guard of the
//!   first kind covers the starter's own death, and the Engine's program is
//!   not executed before that guard is ready (`gate`).
//! - the spawn helper (`spawn_helper`, `spawner`): the MCP shell
//!   (`server/execute.rs`) leads a session of its own (`setsid`), and a guard
//!   started outside that session cannot join its group. One long-lived
//!   helper per Engine process is the parent of those shells instead; it ends
//!   what it still holds when the Engine is gone without a word.
//!
//! Windows is not covered here; nothing in this module is built there. Its
//! containment is in `shell::process_tree`: a kill-on-close Job Object per
//! MCP shell call, and for an SDK session the Engine's own kill-on-close job
//! (`contain_requested_session`), which answers the same request variable
//! with the same acknowledgement.

use std::time::{Duration, Instant};

mod gate;
mod guard;
mod spawn_helper;
mod spawner;
mod starter;
#[cfg(test)]
pub(crate) mod tests;
mod wire;

pub(crate) use gate::{Refusal, start_behind_guard};
#[cfg(test)]
pub(crate) use guard::force_outcome_for_test;
pub(crate) use guard::{CallGuard, GuardOutcome};
pub(crate) use spawn_helper::lead_new_session;
pub(crate) use spawner::{HelpedChild, HelperSpawn, shutdown_spawn_helper, spawn_shell};

/// Whether this process is the `lean-ctx` binary, which is what can be
/// started as a guard. Set by [`dispatch_hidden_mode`], the first thing the
/// binary's entry point calls; true in this crate's own unit tests, whose
/// binary carries a helper that runs the same guard.
static GUARD_HOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(cfg!(test));

/// Starts an Engine child that is not an agent-tool call (bounded captures,
/// the sandbox runner, the credential reader) behind its guard: the program
/// is executed only once the guard is ready ([`start_behind_guard`]).
/// `command` must ask for a process group of its own and is not to be
/// spawned again.
///
/// One addition to that contract: in a process that is not the `lean-ctx`
/// binary — this crate linked into another executable — there is nothing
/// that could be started as a guard. Such a child is then started exactly as
/// it was before guards existed (`None` for the guard). An agent-tool call
/// does not get this exception; it uses [`start_behind_guard`] itself.
///
/// Limit, stated plainly: library embedders and every executable other than
/// `lean-ctx` get NO call guard at these sites. Their children are ended by
/// the sites' own timeout and limit paths only; if such a host process dies,
/// nothing ends what it had started. This is logged once per process at debug
/// level.
pub(crate) fn start_guarded_child(
    command: &mut std::process::Command,
) -> Result<(std::process::Child, Option<CallGuard>), Refusal> {
    if !GUARD_HOST.load(std::sync::atomic::Ordering::Relaxed) {
        note_no_guard_host();
        return command
            .spawn()
            .map(|child| (child, None))
            .map_err(Refusal::NotStarted);
    }
    start_behind_guard(command).map(|(child, guard)| (child, Some(guard)))
}

/// Says once per process, at debug level, that this executable cannot start
/// guards or a spawn helper.
fn note_no_guard_host() {
    static NOTED: std::sync::Once = std::sync::Once::new();
    NOTED.call_once(|| {
        tracing::debug!(
            "process guard unavailable: this executable is not lean-ctx; child processes run without a guard"
        );
    });
}

/// Hidden mode: the guard of one process group. `__group-guard --group <pgid>`.
pub(crate) const GUARD_MODE: &str = "__group-guard";
/// Hidden mode: the session starter.
/// `__session-starter [--ack] [--watch-parent] [--control-fd <n>] -- <program> [args…]`.
pub(crate) const STARTER_MODE: &str = "__session-starter";
/// Hidden mode: the spawn helper of one Engine process. `__spawn-helper`, no
/// arguments; standard input is the Engine's socket.
pub(crate) const HELPER_MODE: &str = "__spawn-helper";
/// Set to `1` by an SDK on an `engine …` start: the binary then runs as the
/// session starter of that same command line.
pub(crate) const SESSION_ENV: &str = "LEAN_CTX_SESSION_CONTAINMENT";
/// The starter's acknowledgement: the first line on standard output, before
/// the Engine is started. An Engine without this mode never writes it.
pub(crate) const SESSION_ACK: &[u8] = b"{\"leanctx_session_containment\":1}\n";

/// The command line is not one of the hidden modes' forms.
pub(crate) const EXIT_USAGE: i32 = 64;
/// The topology could not be verified or the group could not be ended.
pub(crate) const EXIT_CONTAINMENT: i32 = 70;

/// What a caller is told when a call's process group could not be confirmed
/// as ended. Fixed text: no process id, no time, nothing of the run (#498).
pub(crate) const END_NOTICE: &str =
    "[lean-ctx: processes started by this command could not be confirmed as ended]";

/// What a caller is told when an MCP shell command ran without the spawn
/// helper: nothing would have ended it had lean-ctx died meanwhile. Fixed
/// text, as above.
pub(crate) const HELPER_NOTICE: &str = "[lean-ctx: this command ran without the process helper; had lean-ctx stopped, nothing would have ended it]";

/// What a caller is told when the spawn helper went away without saying
/// whether it had started the command. Nothing seen afterwards can prove
/// that a command did not run, so it is never started a second time. Fixed
/// text, as above.
pub(crate) const UNCLEAR_NOTICE: &str =
    "[lean-ctx: this command may or may not have run; it was not repeated]";

/// What a caller is told when a command finished and the word that lets what
/// it left running go on could not be handed to the spawn helper: a helper
/// that never read it ends those processes. Fixed text, as above.
pub(crate) const RELEASE_NOTICE: &str = "[lean-ctx: this command finished, but lean-ctx could not confirm that its background processes were left running]";

/// The notice for a guard outcome; `None` when the group is known to be empty.
pub(crate) fn end_notice(outcome: GuardOutcome) -> Option<&'static str> {
    (outcome != GuardOutcome::Ended).then_some(END_NOTICE)
}

/// How long a group may take to become empty once it is being ended.
const END_BUDGET: Duration = Duration::from_secs(5);

/// Runs the hidden mode this invocation asks for, if any, and returns its exit
/// code. Called before any other start-up work: no configuration, no data
/// directory, no logging, no telemetry.
pub(crate) fn dispatch_hidden_mode() -> Option<i32> {
    GUARD_HOST.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut args = std::env::args_os();
    let _program = args.next();
    let first = args.next()?;
    if first == GUARD_MODE {
        return Some(guard::main(args));
    }
    if first == STARTER_MODE {
        return Some(starter::main_explicit(args));
    }
    if first == HELPER_MODE {
        return Some(spawn_helper::main(args));
    }
    if first == "engine" && std::env::var_os(SESSION_ENV).is_some_and(|value| value == "1") {
        // `None`: this binary cannot name itself. It then runs as a plain
        // Engine and writes no acknowledgement, which the SDK reports.
        return starter::main_requested();
    }
    None
}

/// What became of a group that was to be ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupEnd {
    /// No live member was left, seen twice in a row after the last signal.
    Empty,
    /// The signals were sent; the process table could not be read.
    Unconfirmed,
    /// Live members remained when the budget was spent.
    Survivors,
    /// Nothing was signalled: the caller is a member of the group, or the
    /// number is not a group this process could own.
    Refused,
}

/// Ends the group `group` with SIGKILL, again and again until no live member
/// is left — a member that forks while the group is signalled can leave a
/// child the signal did not reach.
///
/// Caller contract (rule 1 above): for the whole call, an unreaped child of
/// this process is a member of `group`. Rule 2 is checked here.
pub(crate) fn end_pinned_group(group: libc::pid_t, budget: Duration) -> GroupEnd {
    const ROUNDS_WITHOUT_SIGHT: u32 = 5;
    // SAFETY: getpgrp has no preconditions.
    if group <= 1 || unsafe { libc::getpgrp() } == group {
        return GroupEnd::Refused;
    }
    let deadline = Instant::now() + budget;
    let mut rounds = 0_u32;
    loop {
        // SAFETY: plain syscall; the group id is pinned by the caller's
        // unreaped child and this process is not a member. The result is not
        // used: an empty group answers ESRCH, Darwin answers EPERM for a group
        // of zombies only.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        rounds += 1;
        let pause = Duration::from_millis(u64::from(rounds.min(10)) * 2);
        // The next signal is sent only after a live member was seen, or —
        // without sight of the process table — for a few rounds on the pin
        // alone. Once the group is seen empty, nothing more is sent.
        std::thread::sleep(pause);
        let seen = live_members(group);
        if seen == Some(0) {
            std::thread::sleep(pause);
            if live_members(group) == Some(0) {
                return GroupEnd::Empty;
            }
        } else if seen.is_none() && rounds >= ROUNDS_WITHOUT_SIGHT {
            return GroupEnd::Unconfirmed;
        }
        if Instant::now() >= deadline {
            return GroupEnd::Survivors;
        }
    }
}

/// How many live (not zombie) processes are members of `group`, read from the
/// process table. `None` when it cannot be read. Sends no signal.
pub(crate) fn live_members(group: libc::pid_t) -> Option<usize> {
    let ps = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .unwrap_or("ps");
    let output = std::process::Command::new(ps)
        .args(["-A", "-o", "pgid=", "-o", "stat="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(count_live(&String::from_utf8_lossy(&output.stdout), group))
}

/// Counts the rows `<pgid> <state>` of `group` whose state is not a zombie's.
fn count_live(table: &str, group: libc::pid_t) -> usize {
    table
        .lines()
        .filter(|line| {
            let mut fields = line.split_whitespace();
            let in_group = fields
                .next()
                .and_then(|pgid| pgid.parse::<libc::pid_t>().ok())
                == Some(group);
            in_group && !fields.next().is_some_and(|state| state.starts_with('Z'))
        })
        .count()
}

/// Writes all of `bytes` to the descriptor, without any buffering of ours.
fn write_fd(fd: libc::c_int, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        // SAFETY: the pointer and length describe `bytes`.
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written > 0 {
            bytes = &bytes[written as usize..];
        } else if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
    true
}

/// The signals a host may send to "everything"; `handler` is `SIG_IGN` or a
/// handler function.
fn set_termination_signals(handler: libc::sighandler_t) {
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: installs a disposition; the handler, where it is one, only
        // stores to an atomic.
        unsafe { libc::signal(signal, handler) };
    }
    // SAFETY: a closed peer must surface as a write error, not end us.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
}
