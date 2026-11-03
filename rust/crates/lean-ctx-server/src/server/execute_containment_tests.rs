// SPDX-License-Identifier: Apache-2.0
//! Q13 containment of MCP shell calls: this layer's executor runs every call
//! through the process helper (`shell::containment`). Every process-level
//! case runs inside an external fixture of the containment test harness —
//! a process that leads a group of its own and ends it as its last act — and
//! keeps a sentinel outside the contained groups that must be alive at the
//! end.

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{
    execute_command_in, execute_command_with_env, execute_command_with_env_captured,
    output_before_timeout_marker,
};
use lean_ctx::shell::containment::tests::{
    alive, break_helper_for_test, external_fixture, external_helper, external_helper_body,
    fail_next_release_send_for_test, fail_next_spawn_send_for_test, helper_pid_for_test,
    kill_helper_for_test, pid_from_file, run_external_fixture, set_helper_fault_for_test,
    temp_dir_text, wait_gone,
};
use lean_ctx::shell::containment::{
    END_NOTICE, HELPER_NOTICE, RELEASE_NOTICE, UNCLEAR_NOTICE, live_members,
};

/// The directory the MCP stand-in writes its pids to.
const STANDIN_DIR_ENV: &str = "LEAN_CTX_TEST_MCP_STANDIN_DIR";
/// Set when the MCP stand-in is to end in order.
const ORDERLY_ENV: &str = "LEAN_CTX_TEST_CONTAINMENT_ORDERLY";

/// The body of an external fixture: a sentinel outside the contained groups
/// must survive `case`.
fn contained(case: impl FnOnce()) {
    let fixture = external_fixture();
    let mut sentinel = Command::new("sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    case();
    assert!(
        alive(sentinel.id() as libc::pid_t),
        "a process outside the contained groups was ended"
    );
    sentinel.kill().unwrap();
    sentinel.wait().unwrap();
    fixture.complete();
}

/// Stands in for an Engine that serves MCP shell calls: one call that leaves
/// a background process and is released, then one that is still running when
/// this process ends — killed by the fixture, or in order (`ORDERLY_ENV`).
#[test]
#[ignore = "helper process of the MCP containment fixtures"]
fn mcp_engine_standin() {
    external_helper_body();
    let dir = std::env::var(STANDIN_DIR_ENV).unwrap();
    let (text, code) = execute_command_in(
        &format!("sleep 20 >/dev/null 2>&1 & echo $! > {dir}/released"),
        &dir,
    );
    assert_eq!(code, 0, "{text}");
    if std::env::var_os(ORDERLY_ENV).is_some() {
        let held = std::path::Path::new(&dir).join("held");
        std::thread::spawn(move || {
            pid_from_file(&held);
            lean_ctx::shell::containment::shutdown_spawn_helper();
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
    let mut engine = external_helper(module_path!(), "mcp_engine_standin");
    engine
        .env(STANDIN_DIR_ENV, &dir_path)
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
        let status = lean_ctx::shell::exec::reap_within(&mut engine, Duration::from_secs(10))
            .expect("it exits");
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
    const QUIET: &str = "printf 'out\\n'";

    let captured =
        execute_command_with_env_captured("printf 'out\\n'; exit 3", cwd, &HashMap::new(), None);
    assert_eq!(captured.notice, Some(HELPER_NOTICE));
    assert!(!captured.output.contains(HELPER_NOTICE));
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
    assert_eq!(notice, Some(HELPER_NOTICE));
    assert!(!output.contains(HELPER_NOTICE), "{output}");

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
    assert_eq!(finished.notice, Some(HELPER_NOTICE));
    let (JobState::Completed { output, .. } | JobState::Cancelled { output }) = finished.state
    else {
        panic!("the job has finished");
    };
    assert!(!output.contains(HELPER_NOTICE), "{output}");
}

/// One helper serves every call; its results are those of the direct path,
/// byte for byte; a dead helper is replaced; without one the call still runs
/// and says so in its first line; a call that loses its helper says so too.
fn helper_call() {
    const SCRIPT: &str = "printf 'out\\n'; printf 'err\\n' >&2; exit 3";
    let cwd = temp_dir_text();

    assert_eq!(helper_pid_for_test(), None, "no helper before a call");
    let helped = execute_command_in(SCRIPT, &cwd);
    assert_eq!(helped.1, 3, "{}", helped.0);
    assert!(helped.0.contains("out") && helped.0.contains("err"));
    assert!(!helped.0.contains(HELPER_NOTICE));
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
        format!("{}\n{}", HELPER_NOTICE, helped.0),
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
    assert_eq!(unreleased.notice, Some(RELEASE_NOTICE));
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
    assert_eq!(text.lines().next(), Some(END_NOTICE), "{text}");
    assert_eq!(std::fs::read_to_string(marker.path()).unwrap(), "ran\n");
}

/// A command is never started a second time unless it is proven that it was
/// not started. The command here writes nothing and appends one line to a
/// file per execution, so the file counts the executions.
fn helper_unclear() {
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
    assert_eq!(text.lines().next(), Some(UNCLEAR_NOTICE), "{text}");
    assert_eq!(runs(), 2, "run once by the dying helper, never again");

    // The helper answers that it executed nothing: proof. The caller starts
    // the command itself, once, and says that no helper held it.
    set_helper_fault_for_test(Some("refuse"));
    let (text, code) = execute_command_in(&script, &cwd);
    assert_eq!(code, 0, "{text}");
    assert_eq!(text.lines().next(), Some(HELPER_NOTICE), "{text}");
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

/// A timeout through the helper ends the whole session of the call.
fn helper_timeout() {
    let cwd = temp_dir_text();
    let marker = tempfile::NamedTempFile::new().unwrap();
    let (text, code) = execute_command_with_env(
        &format!("echo $$ > {}; sleep 60 & sleep 60", marker.path().display()),
        &cwd,
        &HashMap::new(),
        Some(400),
    );
    assert_eq!(code, 124, "{text}");
    assert!(output_before_timeout_marker(&text).is_some());
    assert!(wait_gone(pid_from_file(marker.path())));
}

/// Not a test of behaviour: what a whole MCP call of `true` costs with the
/// helper and on the direct path, one `COST` line each.
fn costs() {
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
        "direct: whole call `true`",
        &mut (0..30).map(|_| run_true(&cwd)).collect(),
    );
    break_helper_for_test(false);
}

// --- fixture bodies ------------------------------------------------------

#[test]
#[ignore = "fixture body of an MCP containment test"]
fn helper_call_fixture() {
    contained(helper_call);
}

#[test]
#[ignore = "fixture body of an MCP containment test"]
fn helper_unclear_fixture() {
    contained(helper_unclear);
}

#[test]
#[ignore = "fixture body of an MCP containment test"]
fn helper_timeout_fixture() {
    contained(helper_timeout);
}

#[test]
#[ignore = "fixture body of an MCP containment test"]
fn helper_engine_killed_fixture() {
    contained(|| helper_engine_gone(false));
}

#[test]
#[ignore = "fixture body of an MCP containment test"]
fn helper_engine_shutdown_fixture() {
    contained(|| helper_engine_gone(true));
}

#[test]
#[ignore = "fixture body of an MCP containment measurement"]
fn costs_fixture() {
    contained(costs);
}

// --- the tests -----------------------------------------------------------

/// Run by hand, with `--ignored --nocapture`, on a machine that is otherwise
/// idle (the engine-side costs are measured in `shell::containment`).
#[test]
#[ignore = "measurement, not a test: prints what an MCP call costs with containment"]
fn mcp_call_containment_costs_on_this_machine() {
    print!("{}", run_external_fixture(module_path!(), "costs_fixture"));
}

#[test]
fn one_helper_serves_mcp_shell_calls_and_is_replaced_or_done_without() {
    run_external_fixture(module_path!(), "helper_call_fixture");
}

#[test]
fn an_mcp_call_timeout_ends_the_whole_session_of_the_call() {
    run_external_fixture(module_path!(), "helper_timeout_fixture");
}

#[test]
fn a_killed_engine_leaves_the_helper_to_end_its_running_shell() {
    run_external_fixture(module_path!(), "helper_engine_killed_fixture");
}

#[test]
fn an_orderly_shutdown_has_the_helper_end_nothing() {
    run_external_fixture(module_path!(), "helper_engine_shutdown_fixture");
}

#[test]
fn a_command_is_never_started_twice_without_proof_that_it_did_not_run() {
    run_external_fixture(module_path!(), "helper_unclear_fixture");
}
