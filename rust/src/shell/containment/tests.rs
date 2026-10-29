// SPDX-License-Identifier: Apache-2.0
//! Q13 containment tests.
//!
//! Every process-level case runs inside a FIXTURE: a helper process that
//! leads a process group of its own and ends that group as its last act. A
//! regression that signals the wrong group can therefore only end the
//! fixture, never the test run. Each fixture also keeps a sentinel that
//! belongs to none of the contained groups and must be alive at the end.
//!
//! The helper processes are this test binary itself, started on the ignored
//! test [`containment_helper`]; the guard and the starter run the same
//! functions the hidden modes of the `lean-ctx` binary run.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use super::gate::last_gated_for_test;
pub(crate) use super::gate::{GuardFault, set_guard_fault_for_test};
use super::{
    CallGuard, GroupEnd, GuardOutcome, Refusal, end_pinned_group, live_members, start_behind_guard,
};
use crate::shell::exec::{reap_within, wait_with_limits_captured};

pub(super) const HELPER_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_HELPER";
pub(super) const GROUP_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_GROUP";
pub(super) const HELPER_TEST: &str = "shell::containment::tests::containment_helper";
const SCRIPT_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_SCRIPT";
const CONTROL_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_CONTROL_FD";
const WATCH_PARENT_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_WATCH_PARENT";
const SAME_GROUP_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_SAME_GROUP";
pub(super) const PAUSE_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_PAUSE";
pub(super) const FAULT_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_HELPER_FAULT";

/// A group id no process can have: joining it always fails.
pub(super) const NO_SUCH_GROUP: libc::pid_t = libc::pid_t::MAX;
/// A stand-in Engine that leaves a SIGTERM-ignoring descendant and exits 7.
const LEAVES_BEHIND: &str =
    "trap '' TERM; sleep 60 & echo \"ENGINE $$\"; echo \"DESCENDANT $!\"; exit 7";
/// A stand-in Engine that keeps running with one descendant.
const RUNS: &str = "sleep 60 & echo \"ENGINE $$\"; echo \"DESCENDANT $!\"; sleep 60";
/// A call whose members keep forking for a while.
const STORM: &str = "i=0; while [ \"$i\" -lt 200 ]; do sleep 30 & i=$((i+1)); done; wait";

fn helper(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([HELPER_TEST, "--exact", "--ignored", "--nocapture"])
        .env(HELPER_ENV, mode);
    command
}

#[test]
#[ignore = "subprocess helper for the containment tests"]
fn containment_helper() {
    let mode = std::env::var(HELPER_ENV).expect("helper mode");
    match mode.as_str() {
        "group-guard" => {
            let group = std::env::var(GROUP_ENV).unwrap().parse().unwrap();
            std::process::exit(super::guard::run(group));
        }
        "session-starter" => std::process::exit(super::starter::run(&starter_options())),
        "spawn-helper" => std::process::exit(super::spawn_helper::serve(libc::STDIN_FILENO)),
        "engine-standin" => engine_standin(),
        "mcp-engine-standin" => mcp_engine_standin(),
        "capture-caller" => capture_caller(),
        "starter-parent" => starter_parent(),
        other => fixture(other.strip_prefix("fixture:").expect("known helper mode")),
    }
}

fn starter_options() -> super::starter::Options {
    super::starter::Options {
        ack: true,
        watch_parent: std::env::var_os(WATCH_PARENT_ENV).is_some(),
        control_fd: std::env::var(CONTROL_ENV)
            .ok()
            .map(|fd| fd.parse().unwrap()),
        program: "sh".into(),
        args: vec!["-c".into(), std::env::var_os(SCRIPT_ENV).unwrap()],
        engine_in_own_group: std::env::var_os(SAME_GROUP_ENV).is_none(),
    }
}

/// Stands in for an Engine in the middle of a guarded call: it starts the
/// call behind its guard, names the call's group and then never returns.
#[allow(clippy::zombie_processes)]
fn engine_standin() {
    let (child, _guard) = guarded_call(&std::env::var(SCRIPT_ENV).unwrap(), false);
    println!("CALL-GROUP {}", child.id());
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Stands in for an SDK process: it starts a starter that watches its parent
/// and then never returns.
#[allow(clippy::zombie_processes)]
fn starter_parent() {
    let _starter = helper("session-starter")
        .env(WATCH_PARENT_ENV, "1")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn gone(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Whether the group has no live member left within five seconds. Reads the
/// process table only.
fn wait_gone(group: libc::pid_t) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while live_members(group) != Some(0) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

/// The command of a call as the Engine builds one: `sh -c`, in a process
/// group of its own.
fn call_command(script: &str, piped: bool) -> Command {
    let output = || {
        if piped { Stdio::piped() } else { Stdio::null() }
    };
    let mut command = Command::new("sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(output())
        .stderr(output())
        .process_group(0);
    command
}

/// A call as the Engine starts one — behind the gate, the path every product
/// site takes — and its guard.
fn guarded_call(script: &str, piped: bool) -> (Child, CallGuard) {
    start_behind_guard(&mut call_command(script, piped))
        .unwrap_or_else(|_| panic!("the call starts behind its guard"))
}

/// Reads lines until one starts with `prefix`; returns the number after it.
fn read_marker(lines: &mut BufReader<ChildStdout>, prefix: &str) -> libc::pid_t {
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            lines.read_line(&mut line).unwrap(),
            0,
            "no line `{prefix}…`"
        );
        if let Some(value) = line.trim_end().strip_prefix(prefix) {
            return value.parse().unwrap();
        }
    }
}

/// Reads lines until one equals `expected`.
fn read_line_equal(lines: &mut BufReader<ChildStdout>, expected: &[u8]) {
    let mut line = Vec::new();
    loop {
        line.clear();
        assert_ne!(
            lines.read_until(b'\n', &mut line).unwrap(),
            0,
            "line missing"
        );
        if line == expected {
            return;
        }
    }
}

fn spawn_starter(
    script: &str,
    configure: impl FnOnce(&mut Command),
) -> (Child, BufReader<ChildStdout>) {
    let mut command = helper("session-starter");
    command
        .env(SCRIPT_ENV, script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    configure(&mut command);
    let mut starter = command.spawn().unwrap();
    let lines = BufReader::new(starter.stdout.take().unwrap());
    (starter, lines)
}

const EXTERNAL_FIXTURE: &str = "external-fixture";
const EXTERNAL_COMPLETE: &str = "FIXTURE-COMPLETE external";

/// For process tests of other modules: runs the ignored test `module::name`
/// of this binary as a fixture — in a process group of its own, bounded, with
/// the verdict line as the result. `module` is `module_path!()` of the caller.
/// The fixture body must start with [`external_fixture`] and end with
/// [`ExternalFixture::complete`].
pub(crate) fn run_external_fixture(module: &str, name: &str) {
    // Test names carry no crate name.
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            format!("{module}::{name}").as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(HELPER_ENV, EXTERNAL_FIXTURE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().unwrap();
    let captured = wait_with_limits_captured(child, 256 * 1024, Duration::from_mins(3), true);
    let stdout = String::from_utf8_lossy(&captured.output.stdout);
    let stderr = String::from_utf8_lossy(&captured.output.stderr);
    assert!(
        stdout.contains(EXTERNAL_COMPLETE),
        "fixture status {:?}\n{stdout}\n{stderr}",
        captured.output.status
    );
}

const CALLER_STANDIN: &str = "caller-standin";

/// For the per-site tests of other modules, inside an external fixture: runs
/// the ignored test `module::standin` of this binary as a stand-in caller
/// whose site starts a command that would leave a mark and keep running
/// (the mark's path is [`caller_standin_marker`]). The stand-in is held
/// between "the command's process exists" and "attach its guard" and is
/// SIGKILLed exactly there. Asserted: nothing of the command is left, and its
/// program never ran — there is no mark.
pub(crate) fn caller_killed_before_its_guard(module: &str, standin: &str) {
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    // The stand-in may keep what else it needs beside the mark; the
    // directory is removed here, whatever became of the stand-in.
    let scratch = tempfile::tempdir().unwrap();
    let marker = scratch.path().canonicalize().unwrap().join("ran");
    std::fs::write(&marker, b"").unwrap();
    let mut caller = Command::new(std::env::current_exe().unwrap());
    caller
        .args([
            format!("{module}::{standin}").as_str(),
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env(HELPER_ENV, CALLER_STANDIN)
        .env(SCRIPT_ENV, &marker)
        .env(PAUSE_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut caller = caller.spawn().unwrap();
    let mut lines = BufReader::new(caller.stdout.take().unwrap());
    let group = read_marker(&mut lines, "FORKED ");
    caller.kill().unwrap();
    caller.wait().unwrap();
    assert!(
        wait_gone(group),
        "a command without a guard was left running"
    );
    assert!(
        std::fs::read_to_string(&marker).unwrap().is_empty(),
        "the command's program ran"
    );
}

/// Must be the first statement of a stand-in caller: the file its command is
/// to write its mark to. Refuses to run anywhere but as a stand-in.
pub(crate) fn caller_standin_marker() -> std::path::PathBuf {
    assert_eq!(
        std::env::var(HELPER_ENV).as_deref(),
        Ok(CALLER_STANDIN),
        "this test body runs only as a stand-in caller"
    );
    std::env::var_os(SCRIPT_ENV)
        .expect("the mark's path")
        .into()
}

/// The drop guard of an external fixture body: ends the fixture's own group,
/// the fixture included, also when an assertion fails.
pub(crate) struct ExternalFixture(#[allow(dead_code)] EndOwnGroup);

/// Must be the first statement of an external fixture body. Refuses to run
/// anywhere but in a fixture process (an ignored test run by hand would
/// otherwise end the group of whoever started it).
pub(crate) fn external_fixture() -> ExternalFixture {
    assert_eq!(
        std::env::var(HELPER_ENV).as_deref(),
        Ok(EXTERNAL_FIXTURE),
        "this test body runs only as a fixture"
    );
    // SAFETY: getpgrp has no preconditions.
    let own = unsafe { libc::getpgrp() };
    assert_eq!(own, std::process::id() as libc::pid_t, "fixture leads");
    ExternalFixture(EndOwnGroup(own))
}

impl ExternalFixture {
    /// Prints the verdict line; the group is ended when `self` is dropped.
    pub(crate) fn complete(self) {
        println!("{EXTERNAL_COMPLETE}");
    }
}

/// Ends the fixture's own group — the fixture included — while it leads it.
struct EndOwnGroup(libc::pid_t);

impl Drop for EndOwnGroup {
    fn drop(&mut self) {
        let _ = std::io::stdout().flush();
        // SAFETY: this process leads the group and is alive.
        unsafe { libc::killpg(self.0, libc::SIGKILL) };
    }
}

fn fixture(name: &str) {
    // SAFETY: getpgrp has no preconditions.
    let own = unsafe { libc::getpgrp() };
    assert_eq!(own, std::process::id() as libc::pid_t, "fixture leads");
    let _end = EndOwnGroup(own);
    let mut sentinel = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    match name {
        "call-timeout" => call_timeout(),
        "engine-killed" => engine_killed(),
        "leader-first" => leader_first(),
        "mid-fork" => mid_fork(),
        "cannot-join" => cannot_join(),
        "starter-engine-exits" => starter_engine_exits(),
        "starter-control" => starter_control(),
        "starter-killed" => starter_killed(),
        "starter-killed-early" => starter_killed_early(),
        "starter-parent-gone" => starter_parent_gone(),
        "starter-own-group" => starter_own_group(),
        "capture-ownership" => capture_ownership(),
        "helper-call" => helper_call(),
        "helper-timeout" => helper_timeout(),
        "helper-unclear" => helper_unclear(),
        "capture-caller-killed" => capture_caller_killed(),
        "helper-engine-killed" => helper_engine_gone(false),
        "helper-engine-shutdown" => helper_engine_gone(true),
        "costs" => costs(),
        other => panic!("unknown fixture: {other}"),
    }
    assert!(
        alive(sentinel.id() as libc::pid_t),
        "a process outside the contained groups was ended"
    );
    sentinel.kill().unwrap();
    sentinel.wait().unwrap();
    println!("FIXTURE-COMPLETE {name}");
}

/// Runs one fixture to its verdict and returns what it wrote.
fn run_fixture(name: &str) -> String {
    let mut command = helper(&format!("fixture:{name}"));
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().unwrap();
    // The fixture ends its own group as its last act, so its exit status is a
    // kill signal by design; the marker line is the verdict.
    let captured = wait_with_limits_captured(child, 256 * 1024, Duration::from_mins(3), true);
    let stdout = String::from_utf8_lossy(&captured.output.stdout);
    let stderr = String::from_utf8_lossy(&captured.output.stderr);
    assert!(
        stdout.contains(&format!("FIXTURE-COMPLETE {name}")),
        "fixture status {:?}\n{stdout}\n{stderr}",
        captured.output.status
    );
    stdout.into_owned()
}

// --- per-call guard -------------------------------------------------------

/// (1) A call runs into its timeout while a grandchild holds its pipes: the
/// whole tree of the call is gone, the guard has ended and is reaped, the
/// sentinel lives.
fn call_timeout() {
    let (child, guard) = guarded_call("sleep 60 & sleep 60", true);
    let group = child.id() as libc::pid_t;
    let captured = wait_with_limits_captured(child, 1024, Duration::from_millis(200), true);
    assert!(captured.completeness.timed_out);
    assert_eq!(guard.finish(), GuardOutcome::Ended);
    assert_eq!(live_members(group), Some(0), "the call's tree is gone");
}

/// (2) The Engine is SIGKILLed in the middle of a call. Nobody is left to
/// run the limit path: the guard alone ends the call's group.
fn engine_killed() {
    let mut engine = helper("engine-standin");
    engine
        .env(SCRIPT_ENV, "trap '' TERM; sleep 60 & sleep 60")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut engine = engine.spawn().unwrap();
    let mut lines = BufReader::new(engine.stdout.take().unwrap());
    let group = read_marker(&mut lines, "CALL-GROUP ");
    assert!(
        live_members(group).is_some_and(|live| live >= 1),
        "the call runs"
    );
    engine.kill().unwrap();
    engine.wait().unwrap();
    assert!(wait_gone(group), "the guard ends the call of a dead Engine");
}

/// (3) The call's leader exits first. Once with a SIGTERM-ignoring child that
/// holds the pipes, once with one that holds nothing — which the limit path
/// alone leaves running, and the guard ends when the call is over.
fn leader_first() {
    let (child, guard) = guarded_call("trap '' TERM; sleep 60 & echo started", true);
    let group = child.id() as libc::pid_t;
    let start = Instant::now();
    let captured = wait_with_limits_captured(child, 1024, Duration::from_secs(30), true);
    assert!(start.elapsed() < Duration::from_secs(8));
    assert!(String::from_utf8_lossy(&captured.output.stdout).contains("started"));
    assert_eq!(guard.finish(), GuardOutcome::Ended);
    assert_eq!(live_members(group), Some(0));

    let (child, guard) = guarded_call(
        "trap '' TERM; sleep 60 >/dev/null 2>&1 & echo started",
        true,
    );
    let group = child.id() as libc::pid_t;
    let captured = wait_with_limits_captured(child, 1024, Duration::from_secs(30), true);
    assert!(!captured.completeness.timed_out);
    assert!(!captured.completeness.reader_incomplete);
    assert!(
        live_members(group).is_some_and(|live| live >= 1),
        "the limit path alone leaves a descendant that holds no pipe"
    );
    assert_eq!(guard.finish(), GuardOutcome::Ended);
    assert_eq!(live_members(group), Some(0));

    // A leader that is gone at once — before a guard could have been
    // attached to a running call. Behind the gate the guard is there before
    // the leader's first instruction, so this is an ordinary call: it is not
    // refused, its status is the leader's, and what it left is ended.
    let (child, guard) = guarded_call("sleep 60 >/dev/null 2>&1 & exit 0", true);
    let group = child.id() as libc::pid_t;
    let captured = wait_with_limits_captured(child, 1024, Duration::from_secs(30), true);
    assert!(captured.output.status.success());
    assert_eq!(guard.finish(), GuardOutcome::Ended);
    assert_eq!(live_members(group), Some(0));
}

/// (4) Members that fork while the group is signalled. Repeated: once with
/// the limit path first, once with the guard alone. Evidence by repetition;
/// the race itself cannot be forced.
fn mid_fork() {
    for _ in 0..10 {
        let (child, guard) = guarded_call(STORM, true);
        let group = child.id() as libc::pid_t;
        let captured = wait_with_limits_captured(child, 1024, Duration::from_millis(30), true);
        assert!(captured.completeness.timed_out);
        assert_eq!(guard.finish(), GuardOutcome::Ended);
        assert_eq!(live_members(group), Some(0));

        let (mut child, guard) = guarded_call(STORM, false);
        let group = child.id() as libc::pid_t;
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(guard.finish(), GuardOutcome::Ended);
        assert_eq!(live_members(group), Some(0));
        child.wait().unwrap();
    }
}

/// (5) No guard, no call — on the path every product site takes (the gate):
/// a guard that cannot join the group, a guard that is not guarding this
/// group, and a process that leads no group of its own. Each is refused; its
/// program was never executed (it would have left a mark and kept running),
/// and its process is gone and reaped when the refusal is returned: it ended
/// at the unopened gate. No signal is sent on any of these paths, so there
/// is nothing a member in the middle of a fork could escape.
fn cannot_join() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    let script = format!("echo ran >> {}; sleep 60", marker.path().display());
    let nothing_ran = |case: &str| {
        let process = last_gated_for_test();
        assert!(process > 1, "{case}: a process waited at the gate");
        assert!(!alive(process), "{case}: the refused process is reaped");
        assert_eq!(live_members(process), Some(0), "{case}: nothing is left");
        assert!(
            std::fs::read_to_string(marker.path()).unwrap().is_empty(),
            "{case}: the refused call's program ran"
        );
    };

    for fault in [GuardFault::CannotJoin, GuardFault::WrongGroup] {
        let mut command = call_command(&script, true);
        set_guard_fault_for_test(Some(fault));
        let refused = start_behind_guard(&mut command);
        set_guard_fault_for_test(None);
        assert!(
            matches!(refused, Err(Refusal::NoGuard(_))),
            "{fault:?}: no guard, no call"
        );
        nothing_ran(&format!("{fault:?}"));
    }

    let mut ungrouped = Command::new("sh");
    ungrouped
        .args(["-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(
        matches!(start_behind_guard(&mut ungrouped), Err(Refusal::NotLeader)),
        "a process that leads no group of its own is not started"
    );
    nothing_ran("not a leader");
}

// --- session starter ------------------------------------------------------

/// (6a) The Engine exits first and leaves a SIGTERM-ignoring descendant that
/// holds the session's output: the starter ends it and only then exits, with
/// the Engine's status. The acknowledgement precedes all Engine output.
fn starter_engine_exits() {
    let (mut starter, mut lines) = spawn_starter(LEAVES_BEHIND, |_| {});
    read_line_equal(&mut lines, super::SESSION_ACK);
    let group = read_marker(&mut lines, "ENGINE ");
    let descendant = read_marker(&mut lines, "DESCENDANT ");
    let status =
        reap_within(&mut starter, Duration::from_secs(15)).expect("the starter exits by itself");
    assert_eq!(status.code(), Some(7), "the Engine's status is passed on");
    assert_eq!(live_members(group), Some(0));
    assert!(gone(descendant));
}

/// (6b) The control descriptor reaches its end: the group is ended, the
/// starter ends the way the Engine was ended.
fn starter_control() {
    use std::os::fd::AsRawFd as _;
    let (reader, writer) = std::io::pipe().unwrap();
    let fd = reader.as_raw_fd();
    // SAFETY: makes our own descriptor inheritable for the starter.
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
    let (mut starter, mut lines) = spawn_starter(RUNS, |command| {
        command.env(CONTROL_ENV, fd.to_string());
    });
    drop(reader);
    let group = read_marker(&mut lines, "ENGINE ");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        starter.try_wait().unwrap().is_none(),
        "the session runs while its control is open"
    );
    assert!(live_members(group).is_some_and(|live| live >= 1));
    drop(writer);
    let status = reap_within(&mut starter, Duration::from_secs(15)).expect("the starter exits");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(live_members(group), Some(0));
}

/// (6c) The starter itself is SIGKILLed: the guard it attached to the
/// Engine's group ends that group.
fn starter_killed() {
    let (mut starter, mut lines) = spawn_starter(RUNS, |_| {});
    let group = read_marker(&mut lines, "ENGINE ");
    starter.kill().unwrap();
    starter.wait().unwrap();
    assert!(wait_gone(group), "the backstop ends the Engine's group");
}

/// (6c′) The starter is SIGKILLed after the Engine's process exists and
/// before the backstop is ready. The Engine's program must not have run:
/// nothing of it is alive and it has written nothing.
fn starter_killed_early() {
    let (mut starter, mut lines) = spawn_starter(RUNS, |command| {
        command.env(PAUSE_ENV, "1");
    });
    let group = read_marker(&mut lines, "FORKED ");
    starter.kill().unwrap();
    starter.wait().unwrap();
    assert!(
        wait_gone(group),
        "an Engine without a backstop was left running"
    );
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut lines, &mut rest).unwrap();
    assert!(
        !rest.contains("ENGINE "),
        "the Engine's program ran: {rest}"
    );
}

/// (6d) The process that started the starter is SIGKILLed: the starter sees
/// that its parent is gone and ends the group.
fn starter_parent_gone() {
    let mut parent = helper("starter-parent");
    parent
        .env(SCRIPT_ENV, RUNS)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut parent = parent.spawn().unwrap();
    let mut lines = BufReader::new(parent.stdout.take().unwrap());
    let group = read_marker(&mut lines, "ENGINE ");
    parent.kill().unwrap();
    parent.wait().unwrap();
    assert!(
        wait_gone(group),
        "a starter without its parent ends the group"
    );
}

/// (6e) Negative: no signal for a group the signaller belongs to. The
/// primitive refuses this fixture's own group — the fixture is alive to say
/// so — and a starter whose Engine did not get a group of its own signals no
/// group, never executes that Engine (it ends at the unopened gate) and
/// reports failure.
fn starter_own_group() {
    // SAFETY: getpgrp has no preconditions.
    let own = unsafe { libc::getpgrp() };
    assert_eq!(
        end_pinned_group(own, Duration::from_secs(1)),
        GroupEnd::Refused
    );
    assert_eq!(
        end_pinned_group(1, Duration::from_secs(1)),
        GroupEnd::Refused
    );
    let (mut starter, _lines) = spawn_starter(RUNS, |command| {
        command.env(SAME_GROUP_ENV, "1");
    });
    let status = reap_within(&mut starter, Duration::from_secs(15)).expect("the starter exits");
    assert_eq!(status.code(), Some(super::EXIT_CONTAINMENT));
}

// --- bounded capture (core::process_capture) ------------------------------

/// The group number printed by a captured `sh` as its first output line.
fn captured_group(stdout: &[u8]) -> libc::pid_t {
    String::from_utf8_lossy(stdout)
        .lines()
        .next()
        .and_then(|line| line.trim().parse().ok())
        .expect("the captured command prints its pid first")
}

/// Q13 steps 1 and 5 for the bounded capture: every group signal is sent
/// while the leader is unreaped — never to a number that a reap has already
/// released for reuse — and nothing of the command is left. Once with a
/// leader that exits first and leaves a SIGTERM-ignoring descendant, once
/// with a timeout. A recycled number cannot be forced; what is asserted is
/// that no signal is sent after the reap (recorder), and that the sentinel
/// and this fixture survive.
fn capture_ownership() {
    use crate::core::process_capture::{spawn_capture, take_group_signal_log, wait_for_capture};
    let capture = |script: &str, timeout: Duration| {
        let mut command = Command::new("sh");
        command.args(["-c", script]).stdin(Stdio::null());
        let _ = take_group_signal_log();
        let captured = wait_for_capture(spawn_capture(&mut command).unwrap(), timeout).unwrap();
        (captured, take_group_signal_log())
    };

    let (captured, signals) = capture(
        "echo $$; trap '' TERM; sleep 60 >/dev/null 2>&1 & exit 0",
        Duration::from_secs(20),
    );
    assert!(!captured.timed_out);
    assert!(captured.output.status.success());
    assert!(!signals.is_empty(), "the leftover's group is signalled");
    assert!(
        signals.iter().all(|unreaped| *unreaped),
        "a group was signalled after its leader was reaped: {signals:?}"
    );
    assert!(wait_gone(captured_group(&captured.output.stdout)));

    let (captured, signals) = capture("echo $$; sleep 60 & sleep 60", Duration::from_millis(300));
    assert!(captured.timed_out);
    assert!(!signals.is_empty());
    assert!(
        signals.iter().all(|unreaped| *unreaped),
        "a group was signalled after its leader was reaped: {signals:?}"
    );
    assert!(wait_gone(captured_group(&captured.output.stdout)));
}

// --- spawn helper (the MCP shell, server/execute.rs) -----------------------

const ORDERLY_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_ORDERLY";

fn temp_dir_text() -> String {
    std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

/// Reads the process id a shell wrote to `path`, once it is there.
fn pid_from_file(path: &std::path::Path) -> libc::pid_t {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "no pid in {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Stands in for an Engine that serves MCP shell calls: one call that leaves
/// a background process and is released, then one that is still running when
/// this process ends — killed by the fixture, or in order (`ORDERLY_ENV`).
fn mcp_engine_standin() {
    use crate::server::execute::execute_command_in;
    let dir = std::env::var(SCRIPT_ENV).unwrap();
    let (text, code) = execute_command_in(
        &format!("sleep 20 >/dev/null 2>&1 & echo $! > {dir}/released"),
        &dir,
    );
    assert_eq!(code, 0, "{text}");
    if std::env::var_os(ORDERLY_ENV).is_some() {
        let held = std::path::Path::new(&dir).join("held");
        std::thread::spawn(move || {
            pid_from_file(&held);
            super::shutdown_spawn_helper();
            std::process::exit(0);
        });
    }
    let _ = execute_command_in(&format!("echo $$ > {dir}/held; sleep 10 & sleep 10"), &dir);
}

/// The Engine dies during a call: the helper ends that call's session, and
/// does not touch what an earlier, released call left running. An Engine
/// that ends in order has the helper end nothing.
fn helper_engine_gone(orderly: bool) {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().canonicalize().unwrap();
    let mut engine = helper("mcp-engine-standin");
    engine
        .env(SCRIPT_ENV, &dir_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if orderly {
        engine.env(ORDERLY_ENV, "1");
    }
    let mut engine = engine.spawn().unwrap();
    let held = pid_from_file(&dir_path.join("held"));
    let released = pid_from_file(&dir_path.join("released"));
    assert!(alive(released), "a released call keeps its background");
    if orderly {
        let status = reap_within(&mut engine, Duration::from_secs(10)).expect("it exits");
        assert!(status.success());
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            live_members(held).is_some_and(|live| live >= 1),
            "an orderly shutdown ends nothing"
        );
    } else {
        assert!(live_members(held).is_some_and(|live| live >= 1));
        engine.kill().unwrap();
        engine.wait().unwrap();
        assert!(
            wait_gone(held),
            "the helper ends the session of a dead Engine's call"
        );
    }
    assert!(
        alive(released),
        "what a released call left running is never signalled"
    );
}

/// Run while no helper can be started, so that every call here has the
/// helper notice. The notice is a field of the result, not a line of its
/// text — on the captured result, through the foreground path `ctx_shell`
/// takes, and on a finished background job, whose retained text is what
/// storage admission, scrubbing and a later poll work on.
fn notice_travels_beside_the_text(cwd: &str, helped: &(String, i32)) {
    use crate::server::background_shell::{
        CapturedForegroundResult, JobState, run_foreground_or_detach_captured, snapshot, start,
    };
    use crate::server::execute::execute_command_with_env_captured;
    use std::collections::HashMap;
    const QUIET: &str = "printf 'out\\n'";

    let captured =
        execute_command_with_env_captured("printf 'out\\n'; exit 3", cwd, &HashMap::new(), None);
    assert_eq!(captured.notice, Some(super::HELPER_NOTICE));
    assert!(!captured.output.contains(super::HELPER_NOTICE));
    assert_eq!(captured.exit_code, helped.1);

    let foreground = run_foreground_or_detach_captured(
        QUIET.to_string(),
        cwd.to_string(),
        HashMap::new(),
        None,
        Duration::from_secs(30),
        None,
        None,
    );
    let CapturedForegroundResult::Finished { output, notice, .. } = foreground else {
        panic!("the foreground call finishes");
    };
    assert_eq!(notice, Some(super::HELPER_NOTICE));
    assert!(!output.contains(super::HELPER_NOTICE), "{output}");

    let job = start(
        format!("{QUIET}; true"),
        cwd.to_string(),
        HashMap::new(),
        None,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let finished = loop {
        let job = snapshot(&job).expect("the job is kept");
        if !matches!(job.state, JobState::Running { .. }) {
            break job;
        }
        assert!(Instant::now() < deadline, "the background job finishes");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(finished.notice, Some(super::HELPER_NOTICE));
    let (JobState::Completed { output, .. } | JobState::Cancelled { output }) = finished.state
    else {
        panic!("the job has finished");
    };
    assert!(!output.contains(super::HELPER_NOTICE), "{output}");
}

/// One helper serves every call; its results are those of the direct path,
/// byte for byte; a dead helper is replaced; without one the call still runs
/// and says so in its first line; a call that loses its helper says so too.
fn helper_call() {
    use super::spawner::{
        break_helper_for_test, fail_next_release_send_for_test, helper_pid_for_test,
        kill_helper_for_test,
    };
    use crate::server::execute::{execute_command_in, execute_command_with_env_captured};
    use std::collections::HashMap;
    const SCRIPT: &str = "printf 'out\\n'; printf 'err\\n' >&2; exit 3";
    let cwd = temp_dir_text();

    assert_eq!(helper_pid_for_test(), None, "no helper before a call");
    let helped = execute_command_in(SCRIPT, &cwd);
    assert_eq!(helped.1, 3, "{}", helped.0);
    assert!(helped.0.contains("out") && helped.0.contains("err"));
    assert!(!helped.0.contains(super::HELPER_NOTICE));
    let first = helper_pid_for_test().expect("the first call starts the helper");
    assert_eq!(execute_command_in(SCRIPT, &cwd), helped);
    assert_eq!(helper_pid_for_test(), Some(first), "one helper, many calls");
    let (text, code) = execute_command_in("definitely-not-a-command-q13", &cwd);
    assert_ne!(code, 0, "{text}");

    kill_helper_for_test();
    assert_eq!(execute_command_in(SCRIPT, &cwd), helped, "a new helper");
    let second = helper_pid_for_test().expect("the helper was started again");
    assert_ne!(second, first);

    kill_helper_for_test();
    break_helper_for_test(true);
    let direct = execute_command_in(SCRIPT, &cwd);
    assert_eq!(direct.1, 3);
    assert_eq!(
        direct.0,
        format!("{}\n{}", super::HELPER_NOTICE, helped.0),
        "the direct path: the notice line, then the same bytes"
    );
    assert_eq!(helper_pid_for_test(), None);
    notice_travels_beside_the_text(&cwd, &helped);
    break_helper_for_test(false);
    assert_eq!(execute_command_in(SCRIPT, &cwd), helped, "a helper again");

    // A release that does not reach the helper: the command finished and its
    // result is whole; the caller is told that what it left running may not
    // have been left running.
    fail_next_release_send_for_test();
    let unreleased = execute_command_with_env_captured(SCRIPT, &cwd, &HashMap::new(), None);
    assert_eq!(unreleased.notice, Some(super::RELEASE_NOTICE));
    assert_eq!((unreleased.output, unreleased.exit_code), helped);
    assert!(unreleased.completeness.is_complete());
    let released = execute_command_with_env_captured(SCRIPT, &cwd, &HashMap::new(), None);
    assert_eq!(
        released.notice, None,
        "a release that was sent adds nothing"
    );

    // The helper dies during a call: the call ends, reports failure and
    // carries the fixed notice; the command is not started a second time.
    let marker = tempfile::NamedTempFile::new().unwrap();
    let script = format!("echo ran >> {}; sleep 3", marker.path().display());
    let running = std::thread::spawn({
        let cwd = cwd.clone();
        move || execute_command_in(&script, &cwd)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while std::fs::read_to_string(marker.path()).unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the command starts");
        std::thread::sleep(Duration::from_millis(20));
    }
    kill_helper_for_test();
    let (text, code) = running.join().unwrap();
    assert_eq!(code, 1, "{text}");
    assert_eq!(text.lines().next(), Some(super::END_NOTICE), "{text}");
    assert_eq!(std::fs::read_to_string(marker.path()).unwrap(), "ran\n");
}

/// Stands in for a process that starts a bounded capture: the command would
/// leave a mark and keep running.
fn capture_caller() {
    use crate::core::process_capture::{spawn_capture, wait_for_capture};
    let mut command = Command::new("sh");
    command
        .args(["-c", &std::env::var(SCRIPT_ENV).unwrap()])
        .stdin(Stdio::null());
    let _ = wait_for_capture(spawn_capture(&mut command).unwrap(), Duration::from_mins(1));
}

/// The caller of a bounded capture is SIGKILLed after the command's process
/// exists and before its guard is ready: the command's program never ran —
/// no mark, nothing alive.
fn capture_caller_killed() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    let mut caller = helper("capture-caller");
    caller
        .env(
            SCRIPT_ENV,
            format!("echo ran > {}; sleep 60", marker.path().display()),
        )
        .env(PAUSE_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut caller = caller.spawn().unwrap();
    let mut lines = BufReader::new(caller.stdout.take().unwrap());
    let group = read_marker(&mut lines, "FORKED ");
    caller.kill().unwrap();
    caller.wait().unwrap();
    assert!(
        wait_gone(group),
        "a command without a guard was left running"
    );
    assert!(
        std::fs::read_to_string(marker.path()).unwrap().is_empty(),
        "the command's program ran"
    );
}

/// A command is never started a second time unless it is proven that it was
/// not started. The command here writes nothing and appends one line to a
/// file per execution, so the file counts the executions.
fn helper_unclear() {
    use super::spawner::{
        fail_next_spawn_send_for_test, helper_pid_for_test, kill_helper_for_test,
        set_helper_fault_for_test,
    };
    use crate::server::execute::execute_command_in;
    let cwd = temp_dir_text();
    let marker = tempfile::NamedTempFile::new().unwrap();
    let script = format!("echo ran >> {}", marker.path().display());
    let runs = || {
        std::fs::read_to_string(marker.path())
            .unwrap()
            .lines()
            .count()
    };

    let (text, code) = execute_command_in(&script, &cwd);
    assert_eq!((code, runs()), (0, 1), "{text}");

    // The helper dies after it started the command and before it said so:
    // the pipes are empty and at their end, and the command DID run. The
    // call fails with the fixed notice and the command is not repeated.
    kill_helper_for_test();
    set_helper_fault_for_test(Some("die-after-start"));
    let (text, code) = execute_command_in(&script, &cwd);
    assert_ne!(code, 0, "{text}");
    assert_eq!(text.lines().next(), Some(super::UNCLEAR_NOTICE), "{text}");
    assert_eq!(runs(), 2, "run once by the dying helper, never again");

    // The helper answers that it executed nothing: proof. The caller starts
    // the command itself, once, and says that no helper held it.
    set_helper_fault_for_test(Some("refuse"));
    let (text, code) = execute_command_in(&script, &cwd);
    assert_eq!(code, 0, "{text}");
    assert_eq!(text.lines().next(), Some(super::HELPER_NOTICE), "{text}");
    assert_eq!(runs(), 3);

    // The helper is dead before the request is sent: proof. A new helper,
    // a normal call, one execution.
    set_helper_fault_for_test(None);
    kill_helper_for_test();
    let (text, code) = execute_command_in(&script, &cwd);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("[lean-ctx:"), "{text}");
    assert_eq!(runs(), 4);

    // The send fails before one byte left: proof. A new helper, one execution.
    let before = helper_pid_for_test().expect("a helper runs");
    fail_next_spawn_send_for_test();
    let (text, code) = execute_command_in(&script, &cwd);
    assert_eq!(code, 0, "{text}");
    assert!(!text.contains("[lean-ctx:"), "{text}");
    assert_eq!(runs(), 5);
    assert_ne!(
        helper_pid_for_test(),
        Some(before),
        "the old helper is not used"
    );
}

/// A timeout through the helper ends the whole session of the call; the
/// helper's group signal is sent while the leader is its unreaped child and
/// the leader is reaped afterwards — no signal by number after the reap.
fn helper_timeout() {
    let cwd = temp_dir_text();
    let marker = tempfile::NamedTempFile::new().unwrap();
    let (text, code) = crate::server::execute::execute_command_with_env(
        &format!("echo $$ > {}; sleep 60 & sleep 60", marker.path().display()),
        &cwd,
        &std::collections::HashMap::new(),
        Some(400),
    );
    assert_eq!(code, 124, "{text}");
    assert!(crate::server::execute::output_before_timeout_marker(&text).is_some());
    assert!(wait_gone(pid_from_file(marker.path())));

    let (signals, group, reaped) =
        super::spawn_helper::kill_path_for_test(super::wire::SpawnSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "sleep 60 & sleep 60".into()],
            env: std::env::vars_os().collect(),
            cwd: None,
        });
    assert_eq!(signals, [true], "one signal, while the leader is unreaped");
    assert!(reaped, "the leader is reaped after the signal");
    assert!(wait_gone(group));
}

/// Not a test of behaviour: measures what containment costs per call on this
/// machine and prints one `COST` line per item. The guard and the helper are
/// this test binary here, which starts more slowly than `lean-ctx`.
fn costs() {
    use super::spawner::{break_helper_for_test, kill_helper_for_test};
    use crate::server::execute::execute_command_in;
    use crate::shell::process_tree::ShellChild;
    let cwd = temp_dir_text();
    let note = |name: &str, samples: &mut Vec<Duration>| {
        samples.sort();
        let total: Duration = samples.iter().sum();
        println!(
            "COST {name}: n={} min={:?} median={:?} mean={:?} max={:?}",
            samples.len(),
            samples[0],
            samples[samples.len() / 2],
            total / samples.len() as u32,
            samples[samples.len() - 1]
        );
    };
    let start_once = || {
        let mut command = Command::new("true");
        command.stdin(Stdio::null());
        let begun = Instant::now();
        let started = ShellChild::start(&mut command).unwrap();
        let elapsed = begun.elapsed();
        let mut child = started.child;
        while child.try_wait().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(1));
        }
        child.release();
        elapsed
    };
    note(
        "helper: first start (incl. helper start)",
        &mut vec![start_once()],
    );
    note(
        "helper: start, steady state",
        &mut (0..50).map(|_| start_once()).collect(),
    );
    let run_true = |cwd: &str| {
        let begun = Instant::now();
        let _ = execute_command_in("true", cwd);
        begun.elapsed()
    };
    note(
        "helper: whole call `true`",
        &mut (0..30).map(|_| run_true(&cwd)).collect(),
    );
    kill_helper_for_test();
    break_helper_for_test(true);
    note(
        "direct: start",
        &mut (0..50).map(|_| start_once()).collect(),
    );
    note(
        "direct: whole call `true`",
        &mut (0..30).map(|_| run_true(&cwd)).collect(),
    );
    break_helper_for_test(false);

    let mut attach = Vec::new();
    let mut finish = Vec::new();
    for _ in 0..20 {
        let begun = Instant::now();
        let (child, guard) = guarded_call("sleep 5", false);
        attach.push(begun.elapsed());
        let _ = wait_with_limits_captured(child, 1024, Duration::ZERO, true);
        let begun = Instant::now();
        assert_eq!(guard.finish(), GuardOutcome::Ended);
        finish.push(begun.elapsed());
    }
    note(
        "call: start behind the gate, guard included (test binary as guard)",
        &mut attach,
    );
    note("call guard: finish", &mut finish);
    let mut table = Vec::new();
    for _ in 0..20 {
        let begun = Instant::now();
        let _ = live_members(1);
        table.push(begun.elapsed());
    }
    note("one process-table read", &mut table);
    let mut captures = Vec::new();
    let mut plain = Vec::new();
    for _ in 0..20 {
        use crate::core::process_capture::{spawn_capture, wait_for_capture};
        let mut command = Command::new("true");
        command.stdin(Stdio::null());
        let begun = Instant::now();
        let _ = wait_for_capture(
            spawn_capture(&mut command).unwrap(),
            Duration::from_secs(20),
        );
        captures.push(begun.elapsed());
        let begun = Instant::now();
        let _ = Command::new("true").stdin(Stdio::null()).output();
        plain.push(begun.elapsed());
    }
    note("bounded capture of `true` (with guard)", &mut captures);
    note("plain output() of `true`", &mut plain);
    let mut bare = Vec::new();
    for _ in 0..20 {
        let begun = Instant::now();
        let _ = helper("fixture:none-such")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        bare.push(begun.elapsed());
    }
    note("start of the test binary itself (not lean-ctx)", &mut bare);
}

// --- the tests ------------------------------------------------------------

/// Run by hand, with `--ignored --nocapture`, on a machine that is otherwise
/// idle: `cargo test --lib containment_costs_on_this_machine -- --ignored --nocapture`.
#[test]
#[ignore = "measurement, not a test: prints what containment costs per call"]
fn containment_costs_on_this_machine() {
    print!("{}", run_fixture("costs"));
}

#[test]
fn one_helper_serves_mcp_shell_calls_and_is_replaced_or_done_without() {
    run_fixture("helper-call");
}

#[test]
fn a_helper_timeout_ends_the_session_and_signals_only_while_unreaped() {
    run_fixture("helper-timeout");
}

#[test]
fn a_killed_engine_leaves_the_helper_to_end_its_running_shell() {
    run_fixture("helper-engine-killed");
}

#[test]
fn an_orderly_shutdown_has_the_helper_end_nothing() {
    run_fixture("helper-engine-shutdown");
}

/// The notice for a call that ran without the helper is one fixed line that
/// carries nothing of the run (#498).
#[test]
fn the_helper_notice_is_one_fixed_content_free_line() {
    for notice in [
        super::HELPER_NOTICE,
        super::UNCLEAR_NOTICE,
        super::RELEASE_NOTICE,
    ] {
        assert!(!notice.chars().any(|c| c.is_ascii_digit()));
        assert!(!notice.contains('\n'));
        assert!(notice.starts_with("[lean-ctx: "));
    }
}

#[test]
fn a_captured_command_never_runs_before_its_guard_is_ready() {
    run_fixture("capture-caller-killed");
}

#[test]
fn a_command_is_never_started_twice_without_proof_that_it_did_not_run() {
    run_fixture("helper-unclear");
}

#[test]
fn a_capture_signals_its_group_only_while_the_leader_is_unreaped() {
    run_fixture("capture-ownership");
}

#[test]
fn call_timeout_ends_the_call_tree_and_its_guard() {
    run_fixture("call-timeout");
}

#[test]
fn a_killed_engine_leaves_the_guard_to_end_the_call() {
    run_fixture("engine-killed");
}

#[test]
fn a_leader_that_exits_first_leaves_nothing_of_the_call() {
    run_fixture("leader-first");
}

#[test]
fn the_group_kill_is_repeated_for_members_that_fork() {
    run_fixture("mid-fork");
}

#[test]
fn a_call_without_a_guard_is_refused_and_never_runs() {
    run_fixture("cannot-join");
}

#[test]
fn the_starter_ends_what_an_engine_leaves_behind() {
    run_fixture("starter-engine-exits");
}

#[test]
fn the_starter_ends_the_group_when_its_control_closes() {
    run_fixture("starter-control");
}

#[test]
fn a_killed_starter_leaves_the_backstop_to_end_the_group() {
    run_fixture("starter-killed");
}

#[test]
fn an_engine_never_runs_before_its_backstop_is_ready() {
    run_fixture("starter-killed-early");
}

#[test]
fn the_starter_ends_the_group_when_its_parent_is_gone() {
    run_fixture("starter-parent-gone");
}

#[test]
fn the_starter_never_signals_a_group_it_belongs_to() {
    run_fixture("starter-own-group");
}

/// An end that is not confirmed is shown to the caller by one fixed line; a
/// confirmed end adds nothing. The text carries nothing of the run (#498).
#[test]
fn an_unconfirmed_end_has_one_fixed_content_free_notice() {
    assert_eq!(super::end_notice(GuardOutcome::Ended), None);
    for outcome in [GuardOutcome::Unconfirmed, GuardOutcome::Failed] {
        assert_eq!(super::end_notice(outcome), Some(super::END_NOTICE));
    }
    assert!(!super::END_NOTICE.chars().any(|c| c.is_ascii_digit()));
    assert!(!super::END_NOTICE.contains('\n'));
}

#[test]
fn live_member_count_ignores_zombies_and_other_groups() {
    let table = "  100 S\n  100 Z\n  100 R+\n 1000 S\n   10 S\n  100 Zs\nnot a row\n";
    assert_eq!(super::count_live(table, 100), 2);
    assert_eq!(super::count_live(table, 7), 0);
}

#[test]
fn starter_arguments_are_parsed_strictly() {
    let parse = |args: &[&str]| super::starter::parse(args.iter().map(std::ffi::OsString::from));
    let options = parse(&[
        "--ack",
        "--watch-parent",
        "--control-fd",
        "5",
        "--",
        "prog",
        "a",
        "b",
    ])
    .unwrap();
    assert!(options.ack && options.watch_parent);
    assert_eq!(options.control_fd, Some(5));
    assert_eq!(options.program, "prog");
    assert_eq!(options.args.len(), 2);
    let refused: [&[&str]; 6] = [
        &[],
        &["prog"],
        &["--"],
        &["--control-fd", "2", "--", "prog"],
        &["--control-fd", "x", "--", "prog"],
        &["--unknown", "--", "prog"],
    ];
    for args in refused {
        assert!(parse(args).is_none(), "{args:?}");
    }
}

#[test]
fn guard_arguments_are_parsed_strictly() {
    let main = |args: &[&str]| super::guard::main(args.iter().map(std::ffi::OsString::from));
    let refused: [&[&str]; 5] = [
        &[],
        &["--group"],
        &["--group", "x"],
        &["--group", "1"],
        &["--group", "5", "extra"],
    ];
    for args in refused {
        assert_eq!(main(args), super::EXIT_USAGE, "{args:?}");
    }
    // A well-formed request for a group this process is not in: refused
    // before anything is forked or signalled.
    assert_eq!(main(&["--group", "2147483647"]), super::EXIT_CONTAINMENT);
}
