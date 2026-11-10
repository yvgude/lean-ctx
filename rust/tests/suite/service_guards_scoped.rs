// SPDX-License-Identifier: Apache-2.0
//! The proxy autostart, the updater's managed-proxy restart and the
//! auto-update schedule act for the default install only: the account's own
//! HOME without `LEAN_CTX_DATA_DIR` or another directory override.
//!
//! An audit of #19 (6 Nov 2026) found three paths without that guard. The
//! service manager addresses `lean-ctx-proxy` and `lean-ctx-autoupdate.timer`
//! by name, and `crontab` is per user and ignores HOME, so a sandbox instance
//! stopped, rewrote or deleted the real user's proxy service and cron entry:
//! `lean-ctx uninstall` with a sandbox HOME stripped the real `lean-ctx …
//! update` crontab line.
//!
//! `systemctl`, `launchctl`, `crontab` and `schtasks` on PATH are fakes that
//! only record their arguments, and the user bus variables are removed, so a
//! regression is seen in the record and never reaches a real service manager.
//! The sandbox plants the unit files and a cron line an unguarded path would
//! act on, and its proxy port is a free one, so even an unguarded port
//! hand-off finds nothing to signal.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const FAKE_CRON_LINE: &str = "0 */6 * * * /usr/local/bin/lean-ctx update --quiet --scheduled";
const NOT_THIS_INSTANCE: &str = "Not this instance";

/// One isolated instance: own HOME, data dir, cwd and PATH with the fakes.
struct Instance {
    root: tempfile::TempDir,
    proxy_port: u16,
}

impl Instance {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        for dir in ["home", "data", "work", "bin"] {
            std::fs::create_dir_all(root.path().join(dir)).expect("instance dir");
        }
        // A port nothing listens on: the sandbox never shares the account's
        // default proxy port, whatever a regression would hand off.
        let proxy_port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("free port")
            .port();
        let instance = Self { root, proxy_port };
        instance.install_fakes();
        instance.plant_services();
        instance
    }

    fn install_fakes(&self) {
        let log = self.service_log();
        for tool in ["systemctl", "launchctl", "crontab", "schtasks"] {
            // `crontab -l` lists a lean-ctx update line, so an unguarded
            // remove or install would go on to write `crontab -`.
            let listing = if tool == "crontab" {
                format!("[ \"$1\" = \"-l\" ] && echo '{FAKE_CRON_LINE}'\n")
            } else {
                String::new()
            };
            let script = self.dir("bin").join(tool);
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\necho \"{tool} $*\" >> '{}'\n{listing}exit 0\n",
                    log.display()
                ),
            )
            .expect("fake service manager");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake");
        }
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_lean-ctx"),
            self.dir("bin").join("lean-ctx"),
        )
        .expect("binary link");
    }

    /// The unit files an unguarded path finds under HOME and then drives by
    /// name through the service manager.
    fn plant_services(&self) {
        for (dir, name) in [
            (".config/systemd/user", "lean-ctx-proxy.service"),
            (".config/systemd/user", "lean-ctx-autoupdate.timer"),
            (".config/systemd/user", "lean-ctx-autoupdate.service"),
            ("Library/LaunchAgents", "com.leanctx.proxy.plist"),
            ("Library/LaunchAgents", "com.leanctx.autoupdate.plist"),
        ] {
            let dir = self.dir("home").join(dir);
            std::fs::create_dir_all(&dir).expect("service dir");
            std::fs::write(dir.join(name), "planted by service_guards_scoped\n")
                .expect("planted service");
        }
    }

    fn planted(&self) -> Vec<PathBuf> {
        let home = self.dir("home");
        vec![
            home.join(".config/systemd/user/lean-ctx-proxy.service"),
            home.join(".config/systemd/user/lean-ctx-autoupdate.timer"),
            home.join("Library/LaunchAgents/com.leanctx.proxy.plist"),
            home.join("Library/LaunchAgents/com.leanctx.autoupdate.plist"),
        ]
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn service_log(&self) -> PathBuf {
        self.root.path().join("service-manager-calls.log")
    }

    fn service_calls(&self) -> String {
        std::fs::read_to_string(self.service_log()).unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = base_command(args, &self.dir("bin"), &self.dir("work"));
        command
            .env("HOME", self.dir("home"))
            .env("LEAN_CTX_DATA_DIR", self.dir("data"))
            .env("LEAN_CTX_PROXY_PORT", self.proxy_port.to_string());
        command.output().expect("spawn lean-ctx")
    }

    fn assert_untouched(&self, step: &str) {
        let calls = self.service_calls();
        assert!(
            calls.is_empty(),
            "{step}: an isolated instance reached the user service manager:\n{calls}"
        );
        for path in self.planted() {
            assert!(
                path.exists(),
                "{step}: {} was removed by another instance",
                path.display()
            );
        }
    }

    /// End a daemon this instance may have started (setup), through its own
    /// data dir only.
    fn stop_own_daemon(&self) {
        if self.dir("data").join("daemon.pid").exists() {
            let _ = self.run(&["serve", "--stop"]);
        }
        // A stopping daemon still runs for a moment; it must be gone before
        // the tempdir is removed under it. Only processes started from this
        // sandbox's own `bin/lean-ctx` are waited for (and ended if need be).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let pids = self.own_processes();
            if pids.is_empty() {
                return;
            }
            if std::time::Instant::now() >= deadline {
                for pid in pids {
                    if lean_ctx::ipc::process::signal_target(pid).is_err() {
                        continue;
                    }
                    let _ = Command::new("kill").arg(pid.to_string()).status();
                }
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// PIDs of the processes whose program is this sandbox's `bin/lean-ctx`
    /// link (a daemon `setup` started through PATH). Linux only: elsewhere
    /// `serve --stop` alone ends it.
    fn own_processes(&self) -> Vec<u32> {
        let program = self.dir("bin").join("lean-ctx");
        let program = program.to_string_lossy();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
                let cmdline = std::fs::read(entry.path().join("cmdline")).ok()?;
                let argv0 = cmdline.split(|b| *b == 0).next()?;
                (String::from_utf8_lossy(argv0) == program).then_some(pid)
            })
            .collect()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop_own_daemon();
    }
}

/// The binary under test with the fakes first on PATH and no route to a
/// real user service manager. The caller sets HOME and the directories.
fn base_command(args: &[&str], bin: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .env("LEAN_CTX_NO_UPDATE_CHECK", "1")
        .env("LEAN_CTX_NO_COCKPIT", "1")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("LEAN_CTX_DATA_DIR")
        .env_remove("LEAN_CTX_STATE_DIR")
        .env_remove("LEAN_CTX_CONFIG_DIR")
        .env_remove("LEAN_CTX_CACHE_DIR")
        .env_remove("LEAN_CTX_PROXY_PORT")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("LEAN_CTX_SESSION_ID");
    command
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn says_not_this_instance(output: &Output) -> bool {
    String::from_utf8_lossy(&output.stdout).contains(NOT_THIS_INSTANCE)
        || String::from_utf8_lossy(&output.stderr).contains(NOT_THIS_INSTANCE)
}

/// Proxy autostart (F1): `proxy enable`, `proxy start --autostart`, `wrap`,
/// `proxy disable` and `proxy status` with a sandbox HOME and data dir never
/// reach the service manager and say why.
#[test]
fn proxy_autostart_commands_leave_the_default_install_alone() {
    let sandbox = Instance::new();

    let set = sandbox.run(&["config", "set", "proxy_enabled", "true"]);
    assert!(set.status.success(), "{}", text(&set));
    sandbox.assert_untouched("config set");

    // The record first: a regression shows the calls it made.
    let autostart = sandbox.run(&["proxy", "start", "--autostart"]);
    sandbox.assert_untouched("proxy start --autostart");
    assert_eq!(autostart.status.code(), Some(1), "{}", text(&autostart));
    assert!(says_not_this_instance(&autostart), "{}", text(&autostart));

    let enable = sandbox.run(&["proxy", "enable"]);
    sandbox.assert_untouched("proxy enable");
    assert!(says_not_this_instance(&enable), "{}", text(&enable));
    assert!(
        String::from_utf8_lossy(&enable.stderr).contains("this instance has no autostart service"),
        "{}",
        text(&enable)
    );

    // Codex always goes through the proxy, so wrap needs a running one.
    let port = format!("--port={}", sandbox.proxy_port);
    let wrap = sandbox.run(&["wrap", "codex", &port]);
    sandbox.assert_untouched("wrap codex");
    assert_eq!(wrap.status.code(), Some(1), "{}", text(&wrap));
    assert!(says_not_this_instance(&wrap), "{}", text(&wrap));

    let status = sandbox.run(&["proxy", "status"]);
    sandbox.assert_untouched(&format!("proxy status: {}", text(&status)));

    let disable = sandbox.run(&["proxy", "disable"]);
    sandbox.assert_untouched("proxy disable");
    assert!(disable.status.success(), "{}", text(&disable));
    assert!(says_not_this_instance(&disable), "{}", text(&disable));
}

/// Update scheduler (F3): every `update --schedule` verb and the scheduled
/// tick that removes an orphaned schedule leave the default install's
/// LaunchAgent, timer and crontab alone; status reports "disabled".
#[test]
fn update_schedule_commands_leave_the_default_install_alone() {
    let sandbox = Instance::new();

    for verb in ["6", "notify", "off"] {
        let output = sandbox.run(&["update", "--schedule", verb]);
        sandbox.assert_untouched(&format!("update --schedule {verb}"));
        assert_eq!(
            output.status.code(),
            Some(1),
            "update --schedule {verb}: {}",
            text(&output)
        );
        assert!(
            says_not_this_instance(&output),
            "update --schedule {verb}: {}",
            text(&output)
        );
    }

    let status = sandbox.run(&["update", "--schedule", "status"]);
    sandbox.assert_untouched("update --schedule status");
    assert!(status.status.success(), "{}", text(&status));
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("Auto-update: disabled"),
        "{}",
        text(&status)
    );

    // The scheduled tick with auto-update off removes the "orphaned"
    // schedule before any network access: for another instance it removes
    // nothing.
    let set = sandbox.run(&["config", "set", "updates.auto_update", "false"]);
    assert!(set.status.success(), "{}", text(&set));
    let tick = sandbox.run(&["update", "--quiet", "--scheduled"]);
    sandbox.assert_untouched("update --quiet --scheduled");
    assert!(tick.status.success(), "{}", text(&tick));
}

/// `setup` with the proxy enabled (F1) installs no proxy service of the
/// default install.
#[test]
fn setup_leaves_the_default_install_alone() {
    let sandbox = Instance::new();

    let set = sandbox.run(&["config", "set", "proxy_enabled", "true"]);
    assert!(set.status.success(), "{}", text(&set));

    let setup = sandbox.run(&["setup", "--non-interactive", "--yes"]);
    sandbox.stop_own_daemon();
    sandbox.assert_untouched(&format!("setup: {}", text(&setup)));
}

/// `uninstall` (F1 + F3) stops and removes neither the proxy service nor the
/// update schedule of the default install; the account's `lean-ctx … update`
/// crontab line in particular survives.
#[test]
fn uninstall_leaves_the_default_install_alone() {
    let sandbox = Instance::new();

    let uninstall = sandbox.run(&["uninstall", "--keep-binary"]);
    sandbox.assert_untouched(&format!("uninstall: {}", text(&uninstall)));
    assert!(uninstall.status.success(), "{}", text(&uninstall));
}

/// The fakes really sit first on PATH and the crontab fake lists a lean-ctx
/// line: an empty record above is evidence, not an accident.
#[test]
fn fake_service_managers_record_calls() {
    let sandbox = Instance::new();
    let listing = run_fake(&sandbox.dir("bin").join("crontab"), &["-l"]);
    assert_eq!(
        String::from_utf8_lossy(&listing.stdout).trim(),
        FAKE_CRON_LINE
    );
    let restart = run_fake(
        &sandbox.dir("bin").join("systemctl"),
        &["--user", "restart", "lean-ctx-proxy"],
    );
    assert!(restart.status.success());
    assert_eq!(
        sandbox.service_calls(),
        "crontab -l\nsystemctl --user restart lean-ctx-proxy\n"
    );
}

/// Run a fake this test just wrote. A parallel test's fork can briefly hold
/// the script's write descriptor (Linux `ETXTBSY`), so a busy file is retried.
fn run_fake(script: &Path, args: &[&str]) -> Output {
    for _ in 0..50 {
        match Command::new(script).args(args).output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result.expect("run fake"),
        }
    }
    panic!("{} stayed busy", script.display());
}

/// The account's home directory from the passwd database (not `$HOME`),
/// which is what makes a process the default install. Linux only, like the
/// one test that needs it (`getent` is not on macOS).
#[cfg(target_os = "linux")]
fn account_home() -> Option<PathBuf> {
    let uid = Command::new("id").arg("-u").output().ok()?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    let entry = Command::new("getent")
        .args(["passwd", &uid])
        .output()
        .ok()?;
    let entry = String::from_utf8_lossy(&entry.stdout).to_string();
    let home = entry.trim().split(':').nth(5)?;
    Some(PathBuf::from(home)).filter(|p| p.is_absolute() && p.is_dir())
}

/// Positive control: the default install (the account's HOME, no directory
/// override) still consults its service manager. `update --schedule status`
/// is read-only; with the fakes first on PATH it lists the (fake) crontab,
/// where another instance, above, reports "disabled" without any call.
#[test]
#[cfg(target_os = "linux")]
fn default_install_still_reaches_the_service_manager() {
    let Some(home) = account_home() else {
        eprintln!("skipped: no passwd home for this account");
        return;
    };
    if home
        .join(".config/systemd/user/lean-ctx-autoupdate.timer")
        .exists()
    {
        // `linux_status` answers from the timer file without a call; the
        // record would be empty for an unrelated reason.
        eprintln!("skipped: the account has an auto-update timer; status reads it without a call");
        return;
    }
    let scratch = tempfile::tempdir().expect("tempdir");
    let bin = scratch.path().join("bin");
    let work = scratch.path().join("work");
    std::fs::create_dir_all(&bin).expect("bin");
    std::fs::create_dir_all(&work).expect("work");
    let log = scratch.path().join("service-manager-calls.log");
    for tool in ["systemctl", "crontab"] {
        let script = bin.join(tool);
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"{tool} $*\" >> '{}'\nexit 0\n",
                log.display()
            ),
        )
        .expect("fake");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    let mut command = base_command(&["update", "--schedule", "status"], &bin, &work);
    command.env("HOME", &home);
    let output = command.output().expect("spawn lean-ctx");
    assert!(output.status.success(), "{}", text(&output));
    assert!(!says_not_this_instance(&output), "{}", text(&output));
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(calls, "crontab -l\n", "{}", text(&output));
}
