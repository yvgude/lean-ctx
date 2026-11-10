// SPDX-License-Identifier: Apache-2.0
//! `serve --stop` and every daemon control command act on the daemon of the
//! active data dir only, found through that dir's PID file and socket.
//!
//! On 5 Nov 2026 `lean-ctx serve --stop` with a scratch `LEAN_CTX_DATA_DIR`
//! stopped the user's real daemon three times: before looking at the data
//! dir it ran `systemctl --user stop lean-ctx-daemon`, which addresses the
//! user-wide unit by name.
//!
//! Each test runs two or three instances side by side, each with its own
//! HOME, data dir and socket. One of them stands in for "a daemon outside the
//! sandbox". `systemctl` and `launchctl` on PATH are fakes that only record
//! their arguments, and the user bus variables are removed, so a regression
//! is seen in the record and can never reach a real service manager.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const DAEMON_START_LIMIT: Duration = Duration::from_secs(60);
const DAEMON_EXIT_LIMIT: Duration = Duration::from_secs(20);

/// A daemon this test started: ended and reaped on every way out, a panic
/// included, so no process outlives the test.
struct OwnedDaemon(Child);

impl OwnedDaemon {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    fn is_running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }

    fn wait_exit(&mut self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if !self.is_running() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One isolated instance: own HOME, data dir, cwd and PATH.
struct Instance {
    root: tempfile::TempDir,
}

impl Instance {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for dir in ["home", "data", "work", "bin"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("instance dir");
        }
        let instance = Self { root };
        let log = instance.service_log();
        for tool in ["systemctl", "launchctl", "crontab", "loginctl"] {
            let script = instance.dir("bin").join(tool);
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\necho \"{tool} $*\" >> '{}'\nexit 0\n",
                    log.display()
                ),
            )
            .expect("fake service manager");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake");
        }
        // Whatever resolves `lean-ctx` on PATH finds the binary under test,
        // never an installed copy.
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_lean-ctx"),
            instance.dir("bin").join("lean-ctx"),
        )
        .expect("binary link");
        instance
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn service_log(&self) -> PathBuf {
        self.root.path().join("service-manager-calls.log")
    }

    /// Every call that reached the (fake) service manager.
    fn service_calls(&self) -> String {
        std::fs::read_to_string(self.service_log()).unwrap_or_default()
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
        command
            .args(args)
            .current_dir(self.dir("work"))
            .stdin(Stdio::null())
            .env("HOME", self.dir("home"))
            .env("LEAN_CTX_DATA_DIR", self.dir("data"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir("bin").display()),
            )
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .env("LEAN_CTX_NO_UPDATE_CHECK", "1")
            .env("LEAN_CTX_NO_COCKPIT", "1")
            // No route to a real user service manager, whatever happens.
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("LEAN_CTX_STATE_DIR")
            .env_remove("LEAN_CTX_CONFIG_DIR")
            .env_remove("LEAN_CTX_CACHE_DIR")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("LEAN_CTX_SESSION_ID");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("spawn lean-ctx")
    }

    /// This instance's daemon, in the foreground under the test's control,
    /// once it has written its PID file and bound its socket.
    fn start_daemon(&self) -> OwnedDaemon {
        let stderr = std::fs::File::create(self.root.path().join("daemon-test-stderr.log"))
            .expect("daemon stderr log");
        // Its own process group, as `spawn_detached` gives the daemon that
        // `serve --daemon` starts: a CLI never signals a process of its own
        // group. `serve_daemon_status_and_stop_from_one_process_group` covers
        // the real `serve --daemon` path.
        let child = self
            .command(&["serve", "--_foreground-daemon"])
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("spawn daemon");
        let mut daemon = OwnedDaemon(child);
        let pid_file = self.dir("data").join("daemon.pid");
        let socket = self.dir("data").join("daemon.sock");
        let deadline = Instant::now() + DAEMON_START_LIMIT;
        loop {
            let recorded = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());
            if recorded == Some(daemon.pid()) && socket.exists() {
                return daemon;
            }
            assert!(
                daemon.is_running(),
                "daemon exited during start: {}",
                std::fs::read_to_string(self.root.path().join("daemon-test-stderr.log"))
                    .unwrap_or_default()
            );
            assert!(Instant::now() < deadline, "daemon did not start in time");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_service_calls(instances: &[&Instance]) {
    for instance in instances {
        let calls = instance.service_calls();
        assert!(
            calls.is_empty(),
            "an isolated instance reached the user service manager:\n{calls}"
        );
    }
}

fn assert_daemon_status(instance: &Instance, pid: u32) {
    let status = instance.run(&["serve", "--status"]);
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(
        out.contains(&format!("Daemon running (PID {pid})")),
        "{}",
        text(&status)
    );
}

/// Two data dirs side by side: `serve --stop` in one stops that daemon and
/// leaves the other running; `daemon stop` then stops the other one.
#[test]
fn stopping_one_data_dir_leaves_the_other_running() {
    let first = Instance::new();
    let second = Instance::new();
    let mut first_daemon = first.start_daemon();
    let mut second_daemon = second.start_daemon();
    assert_ne!(first_daemon.pid(), second_daemon.pid());

    let stop = first.run(&["serve", "--stop"]);
    assert!(stop.status.success(), "{}", text(&stop));
    assert!(
        first_daemon.wait_exit(DAEMON_EXIT_LIMIT),
        "the stopped data dir's daemon still runs: {}",
        text(&stop)
    );
    assert!(
        !first.dir("data").join("daemon.pid").exists(),
        "the stopped daemon's PID file stays behind"
    );

    assert!(
        second_daemon.is_running(),
        "the other data dir's daemon was stopped"
    );
    assert_daemon_status(&second, second_daemon.pid());

    let stop_second = second.run(&["daemon", "stop"]);
    assert!(stop_second.status.success(), "{}", text(&stop_second));
    assert!(
        second_daemon.wait_exit(DAEMON_EXIT_LIMIT),
        "{}",
        text(&stop_second)
    );

    assert_no_service_calls(&[&first, &second]);
}

/// A data dir without a daemon: every stop and control command refuses with
/// a message naming that dir, exits 1 where it would stop something, and the
/// daemon of another data dir (the stand-in for the user's real one) keeps
/// running — also through `stop`, `daemon enable|disable` and `doctor`.
#[test]
fn control_commands_without_a_daemon_refuse_and_touch_nothing() {
    let outside = Instance::new();
    let empty = Instance::new();
    let mut outside_daemon = outside.start_daemon();
    let data_dir = empty.dir("data").display().to_string();

    for args in [&["serve", "--stop"][..], &["daemon", "stop"]] {
        let output = empty.run(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {}", text(&output));
        assert!(
            stderr.contains("No lean-ctx daemon is running for data dir")
                && stderr.contains(&data_dir)
                && stderr.contains("Nothing was stopped"),
            "{args:?}: {}",
            text(&output)
        );
        assert!(
            outside_daemon.is_running(),
            "{args:?} stopped another daemon"
        );
    }

    let status = empty.run(&["serve", "--status"]);
    assert!(
        String::from_utf8_lossy(&status.stdout)
            .contains(&format!("Daemon not running for data dir {data_dir}")),
        "{}",
        text(&status)
    );
    let daemon_status = empty.run(&["daemon", "status"]);
    let daemon_status_out = String::from_utf8_lossy(&daemon_status.stdout);
    assert!(
        daemon_status_out.contains("not running") && daemon_status_out.contains(&data_dir),
        "{}",
        text(&daemon_status)
    );

    // The autostart service belongs to the default install.
    for verb in ["enable", "disable"] {
        let output = empty.run(&["daemon", verb]);
        assert_eq!(
            output.status.code(),
            Some(1),
            "daemon {verb}: {}",
            text(&output)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("belongs to the default install"),
            "daemon {verb}: {}",
            text(&output)
        );
    }

    // The full lifecycle stop sweeps this instance's records only, and doctor
    // only reads.
    let stop_all = empty.run(&["stop"]);
    assert!(stop_all.status.success(), "{}", text(&stop_all));
    let _ = empty.run(&["doctor"]);

    assert!(
        outside_daemon.is_running(),
        "another data dir's daemon was stopped"
    );
    assert_daemon_status(&outside, outside_daemon.pid());
    assert!(
        !empty.dir("data").join("daemon.pid").exists(),
        "a refused stop must not create a daemon record"
    );
    assert_no_service_calls(&[&outside, &empty]);

    let stop_outside = outside.run(&["serve", "--stop"]);
    assert!(stop_outside.status.success(), "{}", text(&stop_outside));
    assert!(outside_daemon.wait_exit(DAEMON_EXIT_LIMIT));
}

/// A daemon that `serve --daemon` started, known only by the PID it recorded:
/// it is not this test's child. Ended on every way out, but only while that
/// PID still runs this instance's `--_foreground-daemon`.
struct RecordedDaemon(u32);

impl RecordedDaemon {
    fn is_daemon(&self) -> bool {
        Command::new("ps")
            .args(["-o", "command=", "-p", &self.0.to_string()])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("--_foreground-daemon"))
    }

    fn wait_exit(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if !self.is_daemon() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

impl Drop for RecordedDaemon {
    fn drop(&mut self) {
        if self.is_daemon()
            && let Ok(pid) = lean_ctx::ipc::process::signal_target(self.0)
        {
            // SAFETY: kill only sends a signal to the checked daemon PID.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

/// Promotion review 2, F1: a script, CI job or app runs `serve --daemon`,
/// `serve --status` and `serve --stop` from ONE process group (this test's
/// own). The detached daemon must have left that group: status says it runs,
/// stop stops it with exit 0, and no service manager is called.
#[test]
fn serve_daemon_status_and_stop_from_one_process_group() {
    let instance = Instance::new();
    let start = instance.run(&["serve", "--daemon"]);
    assert!(start.status.success(), "{}", text(&start));

    let pid_file = instance.dir("data").join("daemon.pid");
    let socket = instance.dir("data").join("daemon.sock");
    let pid = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or_else(|| panic!("no daemon PID recorded: {}", text(&start)));
    let daemon = RecordedDaemon(pid);
    let deadline = Instant::now() + DAEMON_START_LIMIT;
    while !socket.exists() {
        assert!(daemon.is_daemon(), "daemon exited during start");
        assert!(Instant::now() < deadline, "daemon did not bind its socket");
        std::thread::sleep(Duration::from_millis(100));
    }

    assert_daemon_status(&instance, pid);

    let stop = instance.run(&["serve", "--stop"]);
    let stderr = String::from_utf8_lossy(&stop.stderr);
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop));
    assert!(
        stderr.contains(&format!("lean-ctx daemon stopped (PID {pid})"))
            && !stderr.contains("Nothing was stopped"),
        "{}",
        text(&stop)
    );
    assert!(daemon.wait_exit(DAEMON_EXIT_LIMIT), "{}", text(&stop));
    assert!(
        !pid_file.exists(),
        "the stopped daemon's PID file stays behind"
    );

    assert_no_service_calls(&[&instance]);
}

/// The fakes really sit first on PATH: an isolated `systemctl` call would be
/// recorded, so an empty record above is evidence, not an accident.
#[test]
fn fake_service_manager_records_calls() {
    let instance = Instance::new();
    // A parallel test's fork can briefly hold the just-written script's write
    // descriptor (Linux `ETXTBSY`); a busy file is retried.
    let mut attempts = 0;
    let status = loop {
        match Command::new(instance.dir("bin").join("systemctl"))
            .args(["--user", "stop", "lean-ctx-daemon"])
            .status()
        {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 50 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            result => break result.expect("run fake"),
        }
    };
    assert!(status.success());
    assert_eq!(
        instance.service_calls().trim(),
        "systemctl --user stop lean-ctx-daemon"
    );
}
