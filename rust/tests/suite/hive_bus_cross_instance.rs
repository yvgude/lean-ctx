// SPDX-License-Identifier: Apache-2.0

//! Cross-machine leases (C5) and handoffs between two machines against the
//! fake 2.x Hive. Each machine is a throwaway home; `ctx_agent` runs in this
//! process with that machine's directories (under the test env lock), the
//! relays run as their own `lean-ctx hive bus relay` processes.
#![cfg(feature = "http-server")]

use crate::support::fake_hive::{FakeHive, FakeOptions};
use lean_ctx_hive::bus::handoff::Handoffs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

struct Machine {
    home: tempfile::TempDir,
}

const DIRS: [(&str, &str); 4] = [
    ("LEAN_CTX_DATA_DIR", "data"),
    ("LEAN_CTX_CONFIG_DIR", "config"),
    ("LEAN_CTX_STATE_DIR", "state"),
    ("LEAN_CTX_CACHE_DIR", "cache"),
];

impl Machine {
    fn new(fake: &FakeHive) -> Self {
        let machine = Self {
            home: tempfile::tempdir().unwrap(),
        };
        let config = machine.home.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[hive]\nroot_key = \"{}\"\n",
                FakeHive::root_public_key_b64()
            ),
        )
        .unwrap();
        let out = machine.run(
            machine.home.path(),
            &[
                "hive",
                "connect",
                "--url",
                &fake.url,
                "--no-browser",
                "--label",
                "box",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        machine
    }

    fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let home = self.home.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
        command
            .current_dir(dir)
            .env("LEAN_CTX_HIVE_PREVIEW", "1")
            .env("HOME", home)
            .env("LEAN_CTX_PROJECT_ROOT", dir)
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .args(args);
        for (var, sub) in DIRS {
            command.env(var, home.join(sub));
        }
        command
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.command(dir, args).output().unwrap()
    }

    fn relay(&self) -> Child {
        self.command(self.home.path(), &["hive", "bus", "relay", "--for", "90"])
            .spawn()
            .unwrap()
    }

    fn project(&self) -> PathBuf {
        let project = self.home.path().join("payments");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(
            project.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/payments.git\n",
        )
        .unwrap();
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn pay() {}\n").unwrap();
        let out = self.run(&project, &["hive", "sync"]);
        assert!(out.status.success(), "{out:?}");
        project
    }

    fn data(&self) -> PathBuf {
        self.home.path().join("data/hive")
    }

    /// Point this process at the machine (caller holds the env lock and a
    /// [`RestoreDirs`] taken before the first call).
    fn enter(&self) {
        for (var, sub) in DIRS {
            // SAFETY: serialized by `test_env_lock()`, held for the whole test.
            unsafe { std::env::set_var(var, self.home.path().join(sub)) };
        }
    }
}

/// The process's directory variables as they were before the test pointed
/// them at a machine, put back on drop (also on a panic). Without it the
/// last machine's dirs, deleted with its tempdir, stayed in the process and
/// every later test in this binary ran against them: the hermetic sandbox
/// check, and CLI children that inherit the state and config dirs.
struct RestoreDirs(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl RestoreDirs {
    /// Caller holds the env lock, and declares this after its guard so the
    /// restore runs while the lock is still held.
    fn capture() -> Self {
        Self(
            DIRS.iter()
                .map(|(var, _)| (*var, std::env::var_os(var)))
                .collect(),
        )
    }
}

impl Drop for RestoreDirs {
    fn drop(&mut self) {
        for (var, value) in &self.0 {
            // SAFETY: serialized by `test_env_lock()`, still held (see `capture`).
            unsafe {
                match value {
                    Some(value) => std::env::set_var(var, value),
                    None => std::env::remove_var(var),
                }
            }
        }
    }
}

/// `ctx_agent` as an MCP call would run it on `machine` for `agent`.
fn agent(
    machine: &Machine,
    project: &Path,
    action: &str,
    agent: Option<&str>,
    message: Option<&str>,
    category: Option<&str>,
    to: Option<&str>,
) -> String {
    machine.enter();
    lean_ctx_server::tools::ctx_agent::handle(
        action,
        Some("codex"),
        Some("dev"),
        None,
        project.to_str().unwrap(),
        agent,
        message,
        category,
        to,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        None,
    )
}

fn register(machine: &Machine, project: &Path) -> String {
    let out = agent(machine, project, "register", None, None, None, None);
    out.split_whitespace()
        .nth(2)
        .unwrap_or_else(|| panic!("{out}"))
        .to_string()
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn lease_ref(granted: &str) -> String {
    granted
        .split("Lease granted: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("{granted}"))
        .to_string()
}

/// Wait until the Hive requests this process queued have run, then make
/// `agent_id`'s next `ctx_agent` call: it shows what Hive answered.
fn answered(machine: &Machine, project: &Path, agent_id: &str) -> String {
    assert!(
        lean_ctx_hive::background::wait_idle(std::time::Duration::from_secs(30)),
        "the queued Hive requests ran"
    );
    agent(
        machine,
        project,
        "control_status",
        Some(agent_id),
        None,
        None,
        None,
    )
}

#[test]
fn leases_and_handoffs_cross_machines_through_hive() {
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let _dirs = RestoreDirs::capture();
    let fake = FakeHive::start_with(FakeOptions {
        instance_card: true,
        ..FakeOptions::default()
    });
    let (a, b) = (Machine::new(&fake), Machine::new(&fake));
    let (root_a, root_b) = (a.project(), b.project());
    assert_eq!(
        fake.project_ids().len(),
        1,
        "both checkouts are one Hive project"
    );
    let alpha = register(&a, &root_a);
    let beta = register(&b, &root_b);

    let (mut relay_a, mut relay_b) = (a.relay(), b.relay());
    wait_for("both relays", || fake.bus_connected().len() == 2);
    // Each machine sees the other's worker (`ctx_agent list`).
    let remote_worker = |machine: &Machine| {
        std::fs::read(machine.data().join("bus/presence.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|p| p["workers"].as_object()?.keys().next().cloned())
    };
    wait_for("B sees alpha's worker", || remote_worker(&b).is_some());
    wait_for("A sees beta's worker", || remote_worker(&a).is_some());
    let beta_worker = remote_worker(&a).unwrap();
    assert!(beta_worker.starts_with("wkr_"), "{beta_worker}");

    // Leases: one file, two machines, one holder; the service clock decides.
    // Hive is asked off the call: the lease is pending (no token) until
    // Hive's answer, which comes with the agent's next call.
    let granted = agent(
        &a,
        &root_a,
        "lease_acquire",
        Some(&alpha),
        Some("src/lib.rs"),
        None,
        None,
    );
    assert!(granted.contains("scope=pending"), "{granted}");
    assert!(
        !granted.contains("fencing_token"),
        "never printed: {granted}"
    );
    let shown = answered(&a, &root_a, &alpha);
    assert!(shown.contains("granted by Hive"), "{shown}");
    assert!(shown.contains("scope=hive"), "{shown}");
    assert!(!shown.contains("fencing_token"), "never printed: {shown}");
    let token = |machine: &Machine| {
        lean_ctx_hive::bus::leases::Bindings::load(&machine.data())
            .leases
            .values()
            .map(|b| b.fencing_token)
            .max()
    };
    assert_eq!(token(&a), Some(1));
    let abs = root_b.join("src/lib.rs");
    let held = agent(
        &b,
        &root_b,
        "lease_acquire",
        Some(&beta),
        Some(abs.to_str().unwrap()),
        None,
        None,
    );
    assert!(held.contains("scope=pending"), "{held}");
    // The refusal reaches the agent on its next call, once, and the local
    // lease is gone with it.
    let refused = answered(&b, &root_b, &beta);
    assert!(refused.contains("Lease HELD (hive)"), "{refused}");
    assert!(refused.contains(&format!("owner={alpha}")), "{refused}");
    assert!(refused.contains("local lease"), "{refused}");
    assert!(
        !answered(&b, &root_b, &beta).contains("Lease HELD"),
        "shown once"
    );
    fake.advance(chrono::Duration::minutes(11));
    let taken = agent(
        &b,
        &root_b,
        "lease_acquire",
        Some(&beta),
        Some("src/lib.rs"),
        None,
        None,
    );
    assert!(taken.contains("scope=pending"), "{taken}");
    let shown = answered(&b, &root_b, &beta);
    assert!(shown.contains("granted by Hive"), "{shown}");
    assert_eq!(token(&b), Some(2), "fencing order");
    let renew = agent(
        &a,
        &root_a,
        "lease_renew",
        Some(&alpha),
        Some("src/lib.rs"),
        Some(&lease_ref(&granted)),
        None,
    );
    assert!(renew.contains("scope=pending"), "{renew}");
    let renewed = answered(&a, &root_a, &alpha);
    assert!(renewed.contains("Hive no longer holds it"), "{renewed}");
    let released = agent(
        &b,
        &root_b,
        "lease_release",
        Some(&beta),
        Some("src/lib.rs"),
        Some(&lease_ref(&taken)),
        None,
    );
    assert!(released.starts_with("Lease released"), "{released}");
    wait_for("the release reached Hive", || {
        fake.live_bus_leases().is_empty()
    });

    // A handoff to the other machine's worker is a typed message through
    // Hive: it reaches beta, and A remembers it under its message id.
    let offered = agent(
        &a,
        &root_a,
        "handoff",
        Some(&alpha),
        Some("payments: take over"),
        None,
        Some(&beta_worker),
    );
    assert!(offered.starts_with("Handoff offered (hive)"), "{offered}");
    wait_for("the handoff at Hive", || {
        fake.bus_events()
            .iter()
            .any(|m| m["type"] == "handoff" && m["body"]["to_worker"] == beta_worker.as_str())
    });
    wait_for("the offer's message id", || {
        Handoffs::load(&a.data())
            .offers
            .values()
            .any(|o| o.from == alpha && o.msg_id.is_some())
    });
    let received = std::cell::RefCell::new(String::new());
    wait_for("beta reads the handoff", || {
        let read = agent(&b, &root_b, "read", Some(&beta), None, None, None);
        received.borrow_mut().push_str(&read);
        read.contains("payments: take over")
    });
    let received = received.into_inner();
    assert!(
        received.contains("[hive]"),
        "fenced as foreign data: {received}"
    );

    let alpha2 = register(&a, &root_a);
    // A slow Hive never holds the call: pending, then a release while Hive
    // is still asked; the release waits behind the grant and ends it.
    std::fs::write(root_a.join("src/slow.rs"), "pub fn slow() {}\n").unwrap();
    fake.delay_answers(std::time::Duration::from_millis(1500));
    let started = std::time::Instant::now();
    let pending = agent(
        &a,
        &root_a,
        "lease_acquire",
        Some(&alpha2),
        Some("src/slow.rs"),
        None,
        None,
    );
    let released = agent(
        &a,
        &root_a,
        "lease_release",
        Some(&alpha2),
        Some("src/slow.rs"),
        Some(&lease_ref(&pending)),
        None,
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(1000),
        "no tool call waits for Hive: {:?}",
        started.elapsed()
    );
    assert!(pending.contains("scope=pending"), "{pending}");
    assert!(released.contains("Lease released"), "{released}");
    let shown = answered(&a, &root_a, &alpha2);
    assert!(shown.contains("granted by Hive"), "{shown}");
    assert!(
        lean_ctx_hive::bus::leases::Bindings::load(&a.data())
            .leases
            .is_empty(),
        "the grant was released behind it"
    );
    wait_for("the late grant's release reached Hive", || {
        fake.live_bus_leases().is_empty()
    });
    relay_a.kill().ok();
    relay_b.kill().ok();
    relay_a.wait().ok();
    relay_b.wait().ok();

    // Offline: the call answers at once; Hive's absence comes with the next
    // call, and the lease stays local.
    drop(fake);
    let started = std::time::Instant::now();
    let local = agent(
        &a,
        &root_a,
        "lease_acquire",
        Some(&alpha2),
        Some("src/lib.rs"),
        None,
        None,
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(local.contains("scope=pending"), "{local}");
    let offline = answered(&a, &root_a, &alpha2);
    assert!(
        offline.contains("scope=local (Hive unreachable"),
        "{offline}"
    );
    assert!(
        offline.contains("other machines are not excluded"),
        "{offline}"
    );
}

/// Task reads answer from the cache and changes are queued: no `ctx_tasks`
/// call waits for Hive, also a slow one. Hive's answers come with the
/// caller's next call.
#[test]
fn task_calls_never_wait_for_hive() {
    use lean_ctx_hive::background::{wait_idle, with_answers};
    use lean_ctx_hive::tasks::{ListFilter, cache};
    use lean_ctx_protocol::hive::lease::HiveRight;
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let _dirs = RestoreDirs::capture();
    let mut options = crate::support::fake_hive::FakeOptions::default();
    options
        .rights
        .extend([HiveRight::TasksRead, HiveRight::TasksWork]);
    let fake = FakeHive::start_with(options);
    let machine = Machine::new(&fake);
    let root = machine.project();
    let vault = fake.project_ids().into_iter().next().unwrap();
    fake.add_task(&vault, "Explain the retry budget", "A fact on retry.rs");
    machine.enter();
    let caller = cache::caller("tester");
    let quick = |what: &str, started: std::time::Instant| {
        assert!(
            started.elapsed() < std::time::Duration::from_millis(1000),
            "{what} waited for Hive: {:?}",
            started.elapsed()
        );
    };

    fake.delay_answers(std::time::Duration::from_millis(1500));
    let started = std::time::Instant::now();
    let first = cache::list(&root, &ListFilter::default(), &caller).unwrap();
    quick("list", started);
    assert!(first.contains("not loaded"), "{first}");
    let started = std::time::Instant::now();
    let queued = cache::queue(
        &root,
        HiveRight::TasksWork,
        &caller,
        "claim of task #1",
        || Err(lean_ctx_hive::HiveError::Refused("test refusal".into())),
    )
    .unwrap();
    quick("a queued change", started);
    assert!(
        queued.starts_with("Queued for Hive: claim of task #1"),
        "{queued}"
    );

    assert!(wait_idle(std::time::Duration::from_secs(30)));
    let started = std::time::Instant::now();
    let listed = with_answers(
        &caller,
        cache::list(&root, &ListFilter::default(), &caller).unwrap(),
    );
    quick("a cached list", started);
    assert!(
        listed.contains("Hive tasks (claim of task #1): test refusal"),
        "{listed}"
    );
    assert!(listed.contains("Explain the retry budget"), "{listed}");
    // A change outdates the cache: the next read says so and refreshes.
    assert!(listed.contains("a refresh is running"), "{listed}");
    assert!(wait_idle(std::time::Duration::from_secs(30)));
    let fresh = cache::list(&root, &ListFilter::default(), &caller).unwrap();
    assert!(!fresh.contains("a refresh is running"), "{fresh}");
}
