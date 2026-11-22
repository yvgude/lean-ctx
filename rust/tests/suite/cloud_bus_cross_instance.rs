// SPDX-License-Identifier: Apache-2.0

//! Cross-machine leases (C5) and handoffs between two machines against the
//! fake 2.x Cloud. Each machine is a throwaway home; `ctx_agent` runs in this
//! process with that machine's directories (under the test env lock), the
//! relays run as their own `lean-ctx cloud bus relay` processes.
#![cfg(feature = "http-server")]

use crate::support::fake_cloud::{FakeCloud, FakeOptions};
use lean_ctx_cloud::bus::handoff::Handoffs;
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
    fn new(fake: &FakeCloud) -> Self {
        let machine = Self {
            home: tempfile::tempdir().unwrap(),
        };
        let config = machine.home.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[cloud]\nroot_key = \"{}\"\n",
                FakeCloud::root_public_key_b64()
            ),
        )
        .unwrap();
        let out = machine.run(
            machine.home.path(),
            &[
                "cloud",
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
            .env("LEAN_CTX_CLOUD_PREVIEW", "1")
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
        self.command(self.home.path(), &["cloud", "bus", "relay", "--for", "90"])
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
        let out = self.run(&project, &["cloud", "sync"]);
        assert!(out.status.success(), "{out:?}");
        project
    }

    fn data(&self) -> PathBuf {
        self.home.path().join("data/cloud")
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

/// What a lease call that waited for Cloud would wait: its whole budget.
const LEASE_BUDGET: std::time::Duration = lean_ctx_cloud::bus::leases::BUDGET;

/// A call that waited for a Cloud holding its answers would run into its
/// whole client `budget`; one that does not wait ends in a fraction of it.
/// Half the budget is the bound: relative to the blocker, not an absolute
/// latency a loaded runner can miss.
fn assert_no_wait_for_cloud(what: &str, started: std::time::Instant, budget: std::time::Duration) {
    let elapsed = started.elapsed();
    assert!(
        elapsed < budget / 2,
        "{what} waited for LeanCTX Cloud: {elapsed:?} (budget {budget:?})"
    );
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

/// Wait until the Cloud requests this process queued have run, then make
/// `agent_id`'s next `ctx_agent` call: it shows what Cloud answered.
fn answered(machine: &Machine, project: &Path, agent_id: &str) -> String {
    assert!(
        lean_ctx_cloud::background::wait_idle(std::time::Duration::from_secs(30)),
        "the queued LeanCTX Cloud requests ran"
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
fn leases_and_handoffs_cross_machines_through_cloud() {
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let _dirs = RestoreDirs::capture();
    let fake = FakeCloud::start_with(FakeOptions {
        instance_card: true,
        ..FakeOptions::default()
    });
    let (a, b) = (Machine::new(&fake), Machine::new(&fake));
    let (root_a, root_b) = (a.project(), b.project());
    assert_eq!(
        fake.project_ids().len(),
        1,
        "both checkouts are one LeanCTX Cloud project"
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
    // Cloud is asked off the call: the lease is pending (no token) until
    // Cloud's answer, which comes with the agent's next call.
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
    assert!(shown.contains("granted by LeanCTX Cloud"), "{shown}");
    assert!(shown.contains("scope=cloud"), "{shown}");
    assert!(!shown.contains("fencing_token"), "never printed: {shown}");
    let token = |machine: &Machine| {
        lean_ctx_cloud::bus::leases::Bindings::load(&machine.data())
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
    assert!(refused.contains("Lease HELD (cloud)"), "{refused}");
    assert!(refused.contains(&format!("owner={alpha}")), "{refused}");
    assert!(refused.contains("local lease"), "{refused}");
    assert!(
        !answered(&b, &root_b, &beta).contains("Lease HELD"),
        "shown once"
    );
    fake.advance(lean_ctx_time::Duration::minutes(11));
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
    assert!(shown.contains("granted by LeanCTX Cloud"), "{shown}");
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
    assert!(
        renewed.contains("LeanCTX Cloud no longer holds it"),
        "{renewed}"
    );
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
    wait_for("the release reached LeanCTX Cloud", || {
        fake.live_bus_leases().is_empty()
    });

    // A handoff to the other machine's worker is a typed message through
    // Cloud: it reaches beta, and A remembers it under its message id.
    let offered = agent(
        &a,
        &root_a,
        "handoff",
        Some(&alpha),
        Some("payments: take over"),
        None,
        Some(&beta_worker),
    );
    assert!(offered.starts_with("Handoff offered (cloud)"), "{offered}");
    wait_for("the handoff at LeanCTX Cloud", || {
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
        received.contains("[cloud]"),
        "fenced as foreign data: {received}"
    );

    let alpha2 = register(&a, &root_a);
    // A slow Cloud never holds the call: pending, then a release while Cloud
    // is still asked; the release waits behind the grant and ends it.
    // Cloud answers nothing until both calls returned, so a call that waited
    // for it would take the whole lease budget: each must stay under half.
    std::fs::write(root_a.join("src/slow.rs"), "pub fn slow() {}\n").unwrap();
    fake.hold_answers();
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
    assert_no_wait_for_cloud("lease_acquire", started, LEASE_BUDGET);
    let started = std::time::Instant::now();
    let released = agent(
        &a,
        &root_a,
        "lease_release",
        Some(&alpha2),
        Some("src/slow.rs"),
        Some(&lease_ref(&pending)),
        None,
    );
    assert_no_wait_for_cloud("lease_release", started, LEASE_BUDGET);
    fake.release_answers();
    assert!(pending.contains("scope=pending"), "{pending}");
    assert!(released.contains("Lease released"), "{released}");
    let shown = answered(&a, &root_a, &alpha2);
    assert!(shown.contains("granted by LeanCTX Cloud"), "{shown}");
    assert!(
        lean_ctx_cloud::bus::leases::Bindings::load(&a.data())
            .leases
            .is_empty(),
        "the grant was released behind it"
    );
    wait_for("the late grant's release reached LeanCTX Cloud", || {
        fake.live_bus_leases().is_empty()
    });
    relay_a.kill().ok();
    relay_b.kill().ok();
    relay_a.wait().ok();
    relay_b.wait().ok();

    // Offline: the call answers at once; Cloud's absence comes with the next
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
    assert_no_wait_for_cloud("offline lease_acquire", started, LEASE_BUDGET);
    assert!(local.contains("scope=pending"), "{local}");
    let offline = answered(&a, &root_a, &alpha2);
    assert!(
        offline.contains("scope=local (LeanCTX Cloud unreachable"),
        "{offline}"
    );
    assert!(
        offline.contains("other machines are not excluded"),
        "{offline}"
    );
}

/// Task reads answer from the cache and changes are queued: no `ctx_tasks`
/// call waits for Cloud, also a slow one. Cloud's answers come with the
/// caller's next call.
#[test]
fn task_calls_never_wait_for_cloud() {
    use lean_ctx_cloud::background::{wait_idle, with_answers};
    use lean_ctx_cloud::tasks::{ListFilter, cache};
    use lean_ctx_protocol::cloud::lease::CloudRight;
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let _dirs = RestoreDirs::capture();
    let mut options = crate::support::fake_cloud::FakeOptions::default();
    options
        .rights
        .extend([CloudRight::TasksRead, CloudRight::TasksWork]);
    let fake = FakeCloud::start_with(options);
    let machine = Machine::new(&fake);
    let root = machine.project();
    let vault = fake.project_ids().into_iter().next().unwrap();
    fake.add_task(&vault, "Explain the retry budget", "A fact on retry.rs");
    machine.enter();
    let caller = cache::caller("tester");
    let quick = |what: &str, started: std::time::Instant| {
        assert_no_wait_for_cloud(what, started, lean_ctx_cloud::tasks::BUDGET);
    };

    // Cloud answers nothing until both calls returned: a call that waited for
    // it would take the whole task budget.
    fake.hold_answers();
    let started = std::time::Instant::now();
    let first = cache::list(&root, &ListFilter::default(), &caller).unwrap();
    quick("list", started);
    assert!(first.contains("not loaded"), "{first}");
    let started = std::time::Instant::now();
    let queued = cache::queue(
        &root,
        CloudRight::TasksWork,
        &caller,
        "claim of task #1",
        || Err(lean_ctx_cloud::CloudError::Refused("test refusal".into())),
    )
    .unwrap();
    quick("a queued change", started);
    fake.release_answers();
    assert!(
        queued.starts_with("Queued for LeanCTX Cloud: claim of task #1"),
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
        listed.contains("LeanCTX Cloud tasks (claim of task #1): test refusal"),
        "{listed}"
    );
    assert!(listed.contains("Explain the retry budget"), "{listed}");
    // A change outdates the cache: the next read says so and refreshes.
    assert!(listed.contains("a refresh is running"), "{listed}");
    assert!(wait_idle(std::time::Duration::from_secs(30)));
    let fresh = cache::list(&root, &ListFilter::default(), &caller).unwrap();
    assert!(!fresh.contains("a refresh is running"), "{fresh}");
}

/// #280: a `ctx_tasks propose` goes out at once off the tool call, as
/// claims and results do, not only with the next scheduler pass: another
/// Instance lists the task within 10 s, and no `cloud sync` runs after the
/// proposal (none runs in this test process at all).
#[test]
fn a_proposed_task_reaches_another_instance_without_a_sync() {
    use lean_ctx_cloud::background::{wait_idle, with_answers};
    use lean_ctx_cloud::tasks::cache;
    use lean_ctx_protocol::cloud::lease::CloudRight;
    use lean_ctx_server::server::tool_trait::{McpTool as _, ToolContext};
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let _dirs = RestoreDirs::capture();
    let mut options = FakeOptions::default();
    options.rights.extend([
        CloudRight::TasksRead,
        CloudRight::TasksWork,
        CloudRight::TasksPropose,
    ]);
    let fake = FakeCloud::start_with(options);
    let (a, b) = (Machine::new(&fake), Machine::new(&fake));
    let (root_a, root_b) = (a.project(), b.project());
    let title = "Document the refund retries";

    a.enter();
    let args = serde_json::json!({
        "action": "propose",
        "title": title,
        "acceptance": "The retry budget is documented in the README",
    });
    let ctx = ToolContext {
        project_root: root_a.to_string_lossy().into_owned(),
        ..ToolContext::default()
    };
    let started = std::time::Instant::now();
    let proposed = lean_ctx_server::tools::registered::ctx_tasks::CtxTasksTool
        .handle(args.as_object().unwrap(), &ctx)
        .unwrap()
        .text;
    // Relative to what waiting would cost (the tasks client budget), not an
    // absolute 1 s a loaded runner can miss.
    assert_no_wait_for_cloud("propose", started, lean_ctx_cloud::tasks::BUDGET);

    let deadline = started + std::time::Duration::from_secs(10);
    loop {
        let listed = b.run(&root_b, &["cloud", "tasks", "list"]);
        let text = String::from_utf8_lossy(&listed.stdout).into_owned();
        if text.contains(title) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "B does not list the proposed task 10 s after the proposal ({proposed}): {listed:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        proposed.contains("Queued for LeanCTX Cloud: task proposal"),
        "{proposed}"
    );
    assert!(
        lean_ctx_cloud::outbox::Outbox::at(&a.data())
            .list(lean_ctx_cloud::outbox::CHANNEL_TASKS)
            .is_empty(),
        "the sent proposal left A's outbox"
    );
    // What Cloud answered comes with A's next ctx_tasks call.
    assert!(wait_idle(std::time::Duration::from_secs(30)));
    a.enter();
    let caller = cache::caller("mcp");
    let answer = with_answers(&caller, String::new());
    assert!(answer.contains("Created task #1, open now"), "{answer}");
}
