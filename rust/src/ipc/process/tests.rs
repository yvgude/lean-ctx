// SPDX-License-Identifier: Apache-2.0
use super::*;

/// `stop`/`uninstall` never reach another account's processes: root's
/// launchd (PID 1) is not a candidate for an ordinary user.
#[cfg(target_os = "macos")]
#[test]
fn name_lookup_is_scoped_to_this_user() {
    // SAFETY: getuid has no preconditions and cannot fail.
    if unsafe { libc::getuid() } == 0 {
        return;
    }
    assert!(!find_pids_by_name("launchd").contains(&1));
}

#[test]
fn current_process_identity_is_stable_and_matches() {
    let pid = std::process::id();
    let identity = identity(pid).expect("supported platform exposes process identity");
    assert!(
        !identity.executable.is_empty(),
        "`identity.executable` is empty"
    );
    assert!(matches_identity(pid, &identity));
}

/// #2176: `restart` terminated the MCP servers of running Claude Code
/// sessions on Windows as "orphans". MCP stdio servers, hooks, command
/// runners (#1292) and one-shot CLI calls are never services.
#[test]
fn mcp_servers_hooks_and_runners_are_never_services() {
    for not_a_service in [
        "lean-ctx",
        "lean-ctx mcp",
        "/Users/me/.local/bin/lean-ctx",
        r"C:\Users\me\.local\bin\lean-ctx.exe",
        r#""C:\Users\me\AppData\Local\lean-ctx\lean-ctx.exe""#,
        r#""C:\Program Files\lean-ctx\lean-ctx.exe" mcp"#,
        "lean-ctx hook observe",
        "lean-ctx hook rewrite --agent claude",
        "lean-ctx -c cargo test --lib",
        "/Users/me/.local/bin/lean-ctx -c git commit -m x",
        "/usr/local/bin/lean-ctx exec make",
        "lean-ctx --track npm test",
        "lean-ctx -t pytest",
        "lean-ctx -c lean-ctx serve --_foreground-daemon",
        "lean-ctx restart",
        "lean-ctx stop",
        "lean-ctx daemon start",
        "lean-ctx proxy status",
        "lean-ctx serve --stop",
        "lean-ctx serve --status",
        "lean-ctx serve --daemon",
        "lean-ctx cockpit",
        "lean-ctx cockpit stop",
        "/usr/bin/grep -c lean-ctx",
        "/opt/lean-ctx-tools/other serve",
        "",
    ] {
        assert_eq!(
            classify_service_cmdline(not_a_service),
            None,
            "must never be stopped: {not_a_service}"
        );
    }
}

#[test]
fn background_services_are_classified_by_command_line() {
    for (command, kind) in [
        ("lean-ctx serve --_foreground-daemon", ServiceKind::Daemon),
        (
            "/Users/me/Library/Application Support/lean-ctx/lean-ctx serve --_foreground-daemon",
            ServiceKind::Daemon,
        ),
        (
            r#""C:\Users\me\AppData\Local\Programs\lean-ctx\lean-ctx.exe" serve --_foreground-daemon"#,
            ServiceKind::Daemon,
        ),
        (
            r"C:\Users\me\.local\bin\LEAN-CTX.EXE serve --_foreground-daemon",
            ServiceKind::Daemon,
        ),
        (
            "/Users/me/.local/bin/lean-ctx proxy start --port 4444",
            ServiceKind::Proxy,
        ),
        (
            r#""C:\x\lean-ctx.exe" proxy start --port=4444"#,
            ServiceKind::Proxy,
        ),
        ("lean-ctx serve --port 8080", ServiceKind::HttpServer),
        (
            "lean-ctx cockpit --foreground --no-open --port=3333",
            ServiceKind::Cockpit,
        ),
    ] {
        assert_eq!(classify_service_cmdline(command), Some(kind), "{command}");
    }
}

#[test]
fn restart_sweeps_only_what_it_starts_again() {
    let restartable = ServiceSweep::Restartable;
    assert!(restartable.includes(ServiceKind::Daemon));
    assert!(restartable.includes(ServiceKind::Proxy));
    assert!(!restartable.includes(ServiceKind::HttpServer));
    assert!(!restartable.includes(ServiceKind::Cockpit));
    for kind in [
        ServiceKind::Daemon,
        ServiceKind::Proxy,
        ServiceKind::HttpServer,
        ServiceKind::Cockpit,
    ] {
        assert!(ServiceSweep::All.includes(kind));
    }
}

fn lean_ctx_identity() -> Option<ProcessIdentity> {
    Some(ProcessIdentity {
        start_marker: 7,
        executable: "/Users/me/.local/bin/lean-ctx".to_string(),
    })
}

/// The selection never returns a protected PID (this process, its
/// ancestors), an MCP server, a process with an unreadable identity, or a
/// foreign executable whose arguments mention lean-ctx.
fn candidate(pid: u32, command: &str, live: Option<ProcessIdentity>) -> Candidate {
    Candidate {
        pid,
        command: command.to_string(),
        live,
        recorded: None,
        same_user: true,
    }
}

#[test]
fn selection_keeps_only_owned_services() {
    let me = std::process::id();
    let protected: std::collections::HashSet<u32> = [me, 41].into_iter().collect();
    let daemon = "lean-ctx serve --_foreground-daemon";
    let candidates = vec![
        candidate(me, daemon, lean_ctx_identity()),
        candidate(41, daemon, lean_ctx_identity()),
        candidate(100, "lean-ctx", lean_ctx_identity()),
        candidate(101, "lean-ctx mcp", lean_ctx_identity()),
        candidate(102, daemon, None),
        candidate(
            103,
            "sh -c /Users/me/.local/bin/lean-ctx serve --_foreground-daemon",
            Some(ProcessIdentity {
                start_marker: 9,
                executable: "/bin/sh".to_string(),
            }),
        ),
        candidate(104, daemon, lean_ctx_identity()),
        candidate(105, "lean-ctx proxy start --port 4444", lean_ctx_identity()),
        candidate(106, "lean-ctx cockpit --foreground", lean_ctx_identity()),
        // Another user's daemon (a `/health` answer or record naming it).
        Candidate {
            same_user: false,
            ..candidate(107, daemon, lean_ctx_identity())
        },
        // The record's identity no longer matches: the PID was reused.
        Candidate {
            recorded: Some(ProcessIdentity {
                start_marker: 6,
                executable: "/Users/me/.local/bin/lean-ctx".to_string(),
            }),
            ..candidate(108, daemon, lean_ctx_identity())
        },
    ];

    let restart: Vec<(u32, ServiceKind)> =
        select_services(candidates.clone(), &protected, ServiceSweep::Restartable)
            .into_iter()
            .map(|s| (s.pid, s.kind))
            .collect();
    assert_eq!(
        restart,
        vec![(104, ServiceKind::Daemon), (105, ServiceKind::Proxy)]
    );

    let stop: Vec<u32> = select_services(candidates, &protected, ServiceSweep::All)
        .into_iter()
        .map(|s| s.pid)
        .collect();
    assert_eq!(stop, vec![104, 105, 106]);
}

/// `restart` names what it stopped instead of "Terminated N orphan
/// process(es)", and says nothing when no service was running.
#[test]
fn stop_report_names_each_stopped_service() {
    let service = |pid, kind| ServiceProcess {
        pid,
        kind,
        args: Vec::new(),
        identity: lean_ctx_identity().expect("fixture identity"),
    };
    assert_eq!(ServiceStopReport::default().stopped_line(), None);
    assert_eq!(ServiceStopReport::default().survivors_line(), None);
    let report = ServiceStopReport {
        stopped: vec![
            service(812, ServiceKind::Daemon),
            service(900, ServiceKind::Proxy),
        ],
        survivors: vec![service(901, ServiceKind::Cockpit)],
    };
    assert_eq!(
        report.stopped_line().as_deref(),
        Some("Stopped lean-ctx services: daemon (PID 812), proxy (PID 900).")
    );
    assert_eq!(
        report.survivors_line().as_deref(),
        Some("Could not stop: Cockpit (PID 901).")
    );
    assert_eq!(report.survivor_pids(), vec![901]);
    for line in [report.stopped_line(), report.survivors_line()] {
        assert!(!line.unwrap_or_default().contains("orphan"));
    }
}

/// The command-line reader works on this platform: this test process's
/// own command line is readable (in-process API on Windows, `ps` on Unix).
#[cfg(any(unix, windows))]
#[test]
fn own_command_line_is_readable() {
    let me = std::process::id();
    let lines = command_lines(&[me]);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].0, me);
    assert!(!lines[0].1.trim().is_empty());
}

/// Even a record that names this process (or its parent) never selects
/// it — a stale PID file pointing at the caller cannot make
/// `restart` kill itself.
#[cfg(unix)]
#[test]
fn recorded_own_pid_is_never_selected() {
    let protected = protected_self_pids();
    let owned: Vec<RecordedPid> = protected.iter().copied().map(RecordedPid::bare).collect();
    assert!(find_service_processes(&owned, ServiceSweep::All).is_empty());
}

/// #2176 review #1: a stale record (daemon PID file after a reboot, a
/// `/health` answer) whose PID now belongs to an unrelated process — here
/// a `sleep` child and this test process — is never signalled, by any
/// sweep, with or without a recorded identity.
#[cfg(unix)]
#[test]
fn stale_record_naming_an_unrelated_process_is_never_signalled() {
    let mut sleeper = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep");
    let pid = sleeper.id();
    let me = std::process::id();
    let live = identity(pid).expect("identity of sleep");
    for sweep in [
        ServiceSweep::All,
        ServiceSweep::Restartable,
        ServiceSweep::Only(ServiceKind::Daemon),
    ] {
        let owned = [
            RecordedPid::bare(pid),
            RecordedPid::bare(me),
            RecordedPid {
                pid,
                identity: Some(live.clone()),
            },
        ];
        let report = stop_services(&owned, sweep);
        assert!(report.stopped.is_empty() && report.survivors.is_empty());
    }
    // A recorded identity that does not match is never signalled either.
    let other = ProcessIdentity {
        start_marker: live.start_marker.wrapping_add(1),
        executable: live.executable.clone(),
    };
    assert!(!signal_same_process(pid, &other, StopSignal::Kill));
    assert!(!signal_same_process(me, &live, StopSignal::Kill));
    assert!(
        sleeper.try_wait().expect("try_wait").is_none(),
        "the unrelated process must still run"
    );
    // The matching identity is delivered (pidfd on Linux, re-check on macOS).
    assert!(signal_same_process(pid, &live, StopSignal::Kill));
    let _ = sleeper.wait();
}

/// #2176 review #2: after an update the running image of an old service
/// is reported as `… (deleted)` (Linux) or the rename-aside sidecar
/// (Windows); it is still the lean-ctx executable. Exact shapes only.
#[test]
fn replaced_images_still_count_as_lean_ctx() {
    for (reported, installed) in [
        (
            "/home/me/.local/bin/lean-ctx (deleted)",
            "/home/me/.local/bin/lean-ctx",
        ),
        (r"C:\bin\lean-ctx.old.exe", r"C:\bin\lean-ctx.exe"),
        (r"C:\bin\LEAN-CTX.OLD.EXE", r"C:\bin\lean-ctx.exe"),
        (
            r"C:\bin\lean-ctx.old-4242-1700000000123.exe",
            r"C:\bin\lean-ctx.exe",
        ),
        ("/usr/bin/lean-ctx", "/usr/bin/lean-ctx"),
    ] {
        assert_eq!(installed_executable_path(reported), installed, "{reported}");
        assert!(is_lean_ctx_executable(reported), "{reported}");
    }
    for not_lean_ctx in [
        r"C:\bin\lean-ctx.older.exe",
        r"C:\bin\lean-ctx.old-notes.exe",
        r"C:\bin\lean-ctx.old-1.exe",
        r"C:\bin\lean-ctx.old-1-2-3.exe",
        r"C:\bin\lean-ctx.old.exe.bak",
        r"C:\bin\other.old.exe",
        "/usr/bin/lean-ctx-helper (deleted)",
        "/usr/bin/lean-ctx (deleted) (deleted)",
        "/usr/bin/lean-ctx.old",
    ] {
        assert!(!is_lean_ctx_executable(not_lean_ctx), "{not_lean_ctx}");
    }
}

#[test]
fn same_process_survives_the_update_rename_but_not_pid_reuse() {
    let unix = ProcessIdentity {
        start_marker: 77,
        executable: "/home/me/.local/bin/lean-ctx".to_string(),
    };
    let deleted = ProcessIdentity {
        executable: "/home/me/.local/bin/lean-ctx (deleted)".to_string(),
        ..unix.clone()
    };
    assert!(same_process(&unix, &deleted));
    let windows = ProcessIdentity {
        start_marker: 9,
        executable: r"C:\bin\lean-ctx.exe".to_string(),
    };
    let parked = ProcessIdentity {
        executable: r"C:\bin\lean-ctx.old.exe".to_string(),
        ..windows.clone()
    };
    assert!(same_process(&windows, &parked));
    let reused = ProcessIdentity {
        start_marker: 78,
        ..unix.clone()
    };
    assert!(!same_process(&unix, &reused));
    let elsewhere = ProcessIdentity {
        executable: "/opt/other/lean-ctx".to_string(),
        ..unix.clone()
    };
    assert!(!same_process(&unix, &elsewhere));
}

#[test]
fn unrecorded_services_are_listed_with_the_way_to_stop_them() {
    let service = |pid, kind| ServiceProcess {
        pid,
        kind,
        args: Vec::new(),
        identity: lean_ctx_identity().expect("fixture identity"),
    };
    assert_eq!(unrecorded_services_lines(&[], false), None);
    let services = [
        service(812, ServiceKind::HttpServer),
        service(913, ServiceKind::Proxy),
    ];
    let (what, how) = unrecorded_services_lines(&services, false).expect("lines");
    assert_eq!(
        what,
        "Still running, not started by this instance (left alone): HTTP server (PID 812), proxy (PID 913)."
    );
    assert_eq!(how, "Stop them with: kill 812 913");
    let (_, how) = unrecorded_services_lines(&services, true).expect("lines");
    assert_eq!(how, "Stop them with: taskkill /PID 812 /PID 913");
}

/// #2176 review #4: `restart` starts a proxy it stopped again with the
/// same arguments unless the autostart service brought it back.
#[test]
fn restart_restores_a_stopped_proxy_without_autostart() {
    let service = |kind, command: &str| ServiceProcess {
        pid: 1,
        kind,
        args: service_args(command),
        identity: lean_ctx_identity().expect("fixture identity"),
    };
    let proxy = service(
        ServiceKind::Proxy,
        r#""C:\Program Files\lean-ctx\lean-ctx.exe" proxy start --port=4545"#,
    );
    let daemon = service(ServiceKind::Daemon, "lean-ctx serve --_foreground-daemon");
    let expected: Vec<String> = ["proxy", "start", "--port=4545"].map(String::from).to_vec();
    assert_eq!(
        proxy_respawn_args(&[daemon.clone(), proxy.clone()], false),
        Some(expected)
    );
    assert_eq!(proxy_respawn_args(&[daemon.clone(), proxy], true), None);
    assert_eq!(proxy_respawn_args(&[daemon], false), None);
    assert_eq!(proxy_respawn_args(&[], false), None);
}

#[cfg(unix)]
#[test]
fn ps_output_is_parsed_per_pid() {
    let out = "  812 /usr/bin/lean-ctx serve --_foreground-daemon\n\
               913 /opt/my dir/lean-ctx proxy start --port=4444\n\
               999 something else\n  \n";
    assert_eq!(
        parse_ps_pid_command(out, &[812, 913]),
        vec![
            (
                812,
                "/usr/bin/lean-ctx serve --_foreground-daemon".to_string()
            ),
            (
                913,
                "/opt/my dir/lean-ctx proxy start --port=4444".to_string()
            ),
        ]
    );
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
#[test]
fn own_process_belongs_to_the_current_user() {
    assert!(owned_by_current_user(std::process::id()));
    assert!(!owned_by_current_user(u32::MAX - 1));
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        if unsafe { libc::getuid() } != 0 {
            assert!(!owned_by_current_user(1), "PID 1 belongs to root");
        }
    }
}

#[cfg(unix)]
#[test]
fn run_with_timeout_returns_output_for_fast_command() {
    let mut cmd = std::process::Command::new("echo");
    cmd.arg("hello");
    let out = run_with_timeout(cmd, std::time::Duration::from_secs(5))
        .expect("fast command should complete");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
}

#[cfg(unix)]
#[test]
fn run_with_timeout_kills_slow_command() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    let start = std::time::Instant::now();
    let result = run_with_timeout(cmd, std::time::Duration::from_millis(300));
    assert!(result.is_none(), "slow command must time out");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "timeout must not wait for the full command"
    );
}

/// #714 follow-up: agent harnesses reparent intermediaries to PID 1, so
/// the ppid walk alone misses the outer wrapper — the shared foreground
/// process group must be protected too.
#[cfg(unix)]
#[test]
fn protected_pids_cover_own_process_group() {
    // SAFETY: getpgrp() takes no arguments and cannot fail.
    let pgid = unsafe { libc::getpgrp() };
    // Bracket `protected_self_pids` with two group snapshots. A single
    // snapshot is racy under `cargo test` parallelism: sibling tests spawn
    // short-lived subprocesses (git/ps/pgrep) that transiently join this
    // foreground process group, so a member seen once may already be gone
    // when `protected_self_pids` runs — or appear only afterwards. Only PIDs
    // present in BOTH snapshots were alive across the whole window and must
    // therefore be covered by the protected set.
    let members_before = process_group_pids(pgid);
    let protected = protected_self_pids();
    let members_after = process_group_pids(pgid);
    for pid in members_before {
        if !members_after.contains(&pid) {
            continue; // transient foreign subprocess from a parallel test
        }
        assert!(
            protected.contains(&pid),
            "stable group member {pid} missing from protected set"
        );
    }
}

/// #714: `stop`/`dev-install` running *under* a lean-ctx shell wrapper
/// (`lean-ctx -c … → sh → lean-ctx dev-install`) must not SIGTERM its own
/// ancestor chain — that killed the pipeline mid-run (exit 143) before
/// autostart was re-enabled.
#[test]
fn protected_pids_cover_self_and_ancestors() {
    let protected = protected_self_pids();
    assert!(protected.contains(&std::process::id()));
    #[cfg(unix)]
    {
        // The direct parent (cargo's test runner) must be protected too.
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps runs");
        if let Ok(ppid) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>()
            && ppid > 1
        {
            assert!(
                protected.contains(&ppid),
                "parent {ppid} missing from {protected:?}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn find_pids_never_reports_own_process_tree() {
    // Regardless of what matches by name, the returned set must be
    // disjoint from the protected self/ancestor set (#714).
    let protected = protected_self_pids();
    for pid in find_pids_by_name("lean-ctx") {
        assert!(!protected.contains(&pid), "own tree pid {pid} reported");
    }
}
