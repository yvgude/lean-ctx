// SPDX-License-Identifier: Apache-2.0

//! `lean-ctx hive connect|status|disconnect|link` end to end against the
//! fake Hive service, each run in its own throwaway home.

use crate::support::fake_hive::{FakeHive, FakeOptions};
use base64::Engine as _;
use lean_ctx::core::hive::auth::Session;
use lean_ctx::core::hive::status::report;
use lean_ctx::core::hive::{HiveConfig, HiveStore};
use lean_ctx_protocol::hive::lease::{HivePlan, LeaseState};
use lean_ctx_protocol::hive::status::HiveCliStatusV1;
use std::path::Path;
use std::process::{Command, Output};

struct Machine {
    home: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
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
        machine
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut full = vec!["hive"];
        full.extend_from_slice(args);
        self.run_in(self.home.path(), &full)
    }

    /// Any `lean-ctx` command with `dir` as working directory and project root.
    fn run_in(&self, dir: &Path, args: &[&str]) -> Output {
        let home = self.home.path();
        Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .current_dir(dir)
            .env("LEAN_CTX_HIVE_PREVIEW", "1")
            .env("HOME", home)
            .env("LEAN_CTX_PROJECT_ROOT", dir)
            .env("LEAN_CTX_DATA_DIR", home.join("data"))
            .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
            .env("LEAN_CTX_STATE_DIR", home.join("state"))
            .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .args(args)
            .output()
            .unwrap()
    }

    fn store(&self) -> HiveStore {
        HiveStore::at(
            self.home.path().join("config/hive"),
            self.home.path().join("data/hive"),
        )
    }

    fn status_json(&self) -> HiveCliStatusV1 {
        let output = self.run(&["status", "--json"]);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"))
    }

    fn connect(&self, fake: &FakeHive) -> Output {
        self.run(&[
            "connect",
            "--url",
            &fake.url,
            "--no-browser",
            "--label",
            "test-box",
        ])
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn config_for(fake: &FakeHive) -> HiveConfig {
    HiveConfig {
        url: Some(fake.url.clone()),
        root_key: Some(FakeHive::root_public_key_b64()),
        ..HiveConfig::default()
    }
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// 3.12.0 ships without Hive: without the test preview, `connect` and `link`
/// refuse with one line, exit non-zero and send nothing, even to a loopback
/// Hive; `status` still answers "not connected".
#[test]
fn release_switch_refuses_connect_and_link_without_a_request() {
    if lean_ctx::core::hive::HIVE_AVAILABLE {
        return;
    }
    let fake = FakeHive::start();
    let machine = Machine::new();
    let home = machine.home.path();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .current_dir(home)
            .env_remove("LEAN_CTX_HIVE_PREVIEW")
            .env("HOME", home)
            .env("LEAN_CTX_PROJECT_ROOT", home)
            .env("LEAN_CTX_DATA_DIR", home.join("data"))
            .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
            .env("LEAN_CTX_STATE_DIR", home.join("state"))
            .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .args(args)
            .output()
            .unwrap()
    };
    for args in [
        &["hive", "connect", "--url", &fake.url, "--no-browser"][..],
        &["hive", "connect", "--url", "https://hive.example.com"][..],
        &["hive", "connect"][..],
        &["hive", "link", "acme/payments"][..],
    ] {
        let output = run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert_eq!(
            text(&output.stderr).trim_end(),
            lean_ctx::core::hive::NOT_AVAILABLE,
            "{args:?}"
        );
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
    }
    assert!(fake.requests().is_empty(), "no request may reach a Hive");
    assert!(!home.join(".lean-ctx-hive.json").exists());
    let status = run(&["hive", "status"]);
    assert!(status.status.success(), "{status:?}");
    assert!(text(&status.stdout).contains("not connected"), "{status:?}");
}

#[test]
fn connect_status_disconnect_round_trip() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(!machine.status_json().connected);

    let output = machine.connect(&fake);
    assert!(output.status.success(), "{output:?}");
    let stdout = text(&output.stdout);
    assert!(stdout.contains("FAKE-"), "the user code is shown: {stdout}");
    // Personal, Team, Company: the team's company and the product plan name.
    assert!(
        stdout.contains("Connected test-box to Company Acme Group · Team Acme AG (Team plan)."),
        "{stdout}"
    );

    let status = machine.status_json();
    assert!(status.connected);
    assert_eq!(
        status.instance.as_ref().map(|i| i.label.as_str()),
        Some("test-box")
    );
    assert_eq!(
        status.org.as_ref().map(|o| o.name.as_str()),
        Some("Acme AG")
    );
    assert_eq!(status.url, fake.url);
    assert!(status.last_seen.is_some());
    let raw: serde_json::Value =
        serde_json::from_slice(&machine.run(&["status", "--json"]).stdout).unwrap();
    let mut keys: Vec<&str> = raw
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "available",
            "connected",
            "drafts",
            "filter_packs",
            "instance",
            "last_seen",
            "metrics_v2",
            "org",
            "policy",
            "policy_refusal",
            "url",
            "web_url"
        ]
    );
    assert_eq!(
        raw["drafts"],
        serde_json::json!({"queued": 0, "refused": 0})
    );

    let human = text(&machine.run(&["status"]).stdout);
    assert!(human.contains("Lease:        valid until"), "{human}");
    assert!(
        human.contains("Plan:         Team · 25 engines connected at once"),
        "{human}"
    );
    assert!(
        human.contains("Workspace:    Company Acme Group · Team Acme AG"),
        "{human}"
    );

    let store = machine.store();
    let seed = std::fs::read_to_string(store.config_dir().join("instance.key")).unwrap();
    for request in fake.requests() {
        assert!(
            !request.body.contains(seed.trim()),
            "the instance key never leaves the machine"
        );
    }
    #[cfg(unix)]
    for file in [
        "instance.key",
        "kx.key",
        "instance.json",
        "lease.json",
        "keys.json",
        "token.json",
    ] {
        assert_eq!(mode(&store.config_dir().join(file)), 0o600, "{file}");
    }

    let again = machine.connect(&fake);
    assert!(
        text(&again.stdout).contains("Already connected"),
        "{again:?}"
    );
    assert_eq!(fake.seats_used(), 1);
    // Outside a project (no git remote) sharing is not asked and nothing is
    // recorded; the `[hive]` settings apply as before (E2E 27.10. E3).
    assert!(
        text(&again.stdout).contains("not a project LeanCTX can share"),
        "{again:?}"
    );
    assert!(!text(&again.stdout).contains("not shared"), "{again:?}");
    assert!(!store.config_dir().join("sharing.json").exists());

    let output = machine.run(&["disconnect"]);
    assert!(output.status.success(), "{output:?}");
    assert!(text(&output.stdout).contains("the seat is free"));
    assert_eq!(fake.seats_used(), 0);
    let status = machine.status_json();
    assert!(!status.connected && status.instance.is_none());
}

/// CHECKLIST 2.1.1 + 2.1.3: one `connect` in a project signs in and records
/// the sharing answer for the workspace. The project's first share then
/// waits for the person's `link_vault` yes: no resolve (which would create
/// a vault) and no sync before it. After the yes, connect maps the project;
/// the next connect asks nothing and needs no request for the known vault.
#[test]
fn connect_confirms_sharing_once_and_maps_the_project_here() {
    use lean_ctx::core::hive::consent::broker::Approval;
    use lean_ctx::core::hive::consent::{Broker, Kind, State, Trust};
    let fake = FakeHive::start();
    let machine = Machine::new();
    let repo = machine.home.path().join("payments");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::write(
        repo.join(".git/config"),
        "[remote \"origin\"]\n\turl = git@github.com:acme/payments.git\n",
    )
    .unwrap();
    let connect = |extra: &[&str]| {
        let mut args = vec!["hive", "connect", "--url", &fake.url, "--no-browser"];
        args.extend_from_slice(extra);
        machine.run_in(&repo, &args)
    };
    let resolves = || {
        fake.requests()
            .iter()
            .filter(|r| r.path == "/v1/vaults/resolve")
            .count()
    };

    let first = connect(&["--label", "test-box", "--yes"]);
    assert!(first.status.success(), "{first:?}");
    let stdout = text(&first.stdout);
    assert!(stdout.contains("choose the workspace"), "{stdout}");
    assert!(stdout.contains("From this machine that is:"), "{stdout}");
    assert!(
        stdout.contains("stays on this machine until you confirm"),
        "--yes never answers the first share: {stdout}"
    );
    assert_eq!(resolves(), 0, "no vault is created without the yes");
    let sharing = machine.store().config_dir().join("sharing.json");
    let decided: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sharing).unwrap()).unwrap();
    assert_eq!(decided["shared"], true);
    #[cfg(unix)]
    assert_eq!(mode(&sharing), 0o600);

    let sync = machine.run_in(&repo, &["hive", "sync"]);
    let said = format!("{}{}", text(&sync.stdout), text(&sync.stderr));
    assert!(said.contains("consent_required"), "{said}");
    assert!(
        !said.contains("cns_"),
        "the request id is not printed: {said}"
    );
    assert_eq!(resolves(), 0, "sync waits for the yes too");

    // The person approves in their own terminal (`hive consent approve`);
    // the test stands in for the typed confirmation.
    let store = machine.store();
    let broker = Broker::open(&store, Trust::load(&store, &config_for(&fake)));
    let pending = broker
        .list()
        .unwrap()
        .into_iter()
        .map(|l| l.record)
        .find(|r| r.state == State::Pending)
        .expect("a pending first-share request");
    assert_eq!(pending.kind, Kind::LinkVault);
    assert_eq!(pending.subject["vault"], "new/payments");
    let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    broker
        .approve(
            &pending.consent_id,
            Approval::TtyOnly { approved_at: now },
            now,
        )
        .unwrap();

    let mapped = connect(&[]);
    assert!(mapped.status.success(), "{mapped:?}");
    let stdout = text(&mapped.stdout);
    assert!(
        stdout.contains("This project syncs with the vault "),
        "{stdout}"
    );
    assert_eq!(resolves(), 1);

    let again = connect(&[]);
    assert!(again.status.success(), "{again:?}");
    let stdout = text(&again.stdout);
    assert!(stdout.contains("Already connected"), "{stdout}");
    assert!(
        !stdout.contains("From this machine that is:"),
        "answered once: {stdout}"
    );
    assert!(
        stdout.contains("This project syncs with the vault "),
        "{stdout}"
    );
    assert_eq!(resolves(), 1, "the known vault needs no request");

    assert!(machine.run(&["disconnect"]).status.success());
    assert!(!sharing.exists(), "the answer goes with the enrollment");
}

/// C0 2.1 "Connect and share": against a Hive that offers the device
/// setup, connect names the project before the approval, takes the page's
/// answer from the enrollment (recorded with source `browser`), and `--yes`
/// answers nothing about sharing; the page learns how the step ended.
#[test]
fn connect_takes_the_sharing_answer_from_the_approval_page() {
    for share in [false, true] {
        let fake = FakeHive::start_with(FakeOptions {
            device_setup: Some(share),
            ..FakeOptions::default()
        });
        let machine = Machine::new();
        let repo = git_project(&machine, "payments", "git@github.com:acme/payments.git");
        let out = machine.run_in(
            &repo,
            &[
                "hive",
                "connect",
                "--url",
                &fake.url,
                "--no-browser",
                "--label",
                "test-box",
                "--yes",
            ],
        );
        assert!(out.status.success(), "{out:?}");
        let stdout = text(&out.stdout);
        assert!(
            stdout.contains("confirm what this machine shares"),
            "{stdout}"
        );
        assert!(
            !stdout.contains("From this machine that is:"),
            "the terminal does not ask again: {stdout}"
        );
        let expected = if share {
            "answered in the browser: shared with"
        } else {
            "answered in the browser: not shared"
        };
        assert!(stdout.contains(expected), "{stdout}");

        let requests = fake.requests();
        let setup = requests
            .iter()
            .find(|r| r.path == "/v1/device/setup")
            .expect("the project is announced");
        let body: serde_json::Value = serde_json::from_str(&setup.body).unwrap();
        assert_eq!(body["schema"], "leanctx.hive.device-setup/v1");
        assert_eq!(body["project"]["name"], "payments");
        assert!(
            body["project"]["remote"]
                .as_str()
                .is_some_and(|r| r.contains("acme/payments")),
            "the remote the engine sees: {body}"
        );
        assert!(body["shares"].as_array().is_some_and(|s| !s.is_empty()));
        let result = requests
            .iter()
            .find(|r| r.path == "/v1/device/setup/result")
            .expect("the page learns how the project step ended");
        let result: serde_json::Value = serde_json::from_str(&result.body).unwrap();
        if !share {
            assert_eq!(result["state"], "kept_local");
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r.path == "/v1/vaults/resolve")
                    .count(),
                0,
                "nothing is shared after a no on the page"
            );
        }

        let sharing = machine.store().config_dir().join("sharing.json");
        let decided: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&sharing).unwrap()).unwrap();
        assert_eq!(decided["shared"], share);
        assert_eq!(decided["source"], "browser");
    }
}

/// The owner's engine talks to a 1.x Hive (prod answers 426 to major 2):
/// against the 1.x-only fake, connect probes the instance card once (404),
/// records bus protocol 1, never sends Hive-Protocol 2.x and never opens a
/// v2 bus session.
#[test]
fn against_a_1x_only_hive_the_engine_stays_on_1x() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    let output = machine.connect(&fake);
    assert!(output.status.success(), "{output:?}");
    let requests = fake.requests();
    let probes: Vec<_> = requests
        .iter()
        .filter(|r| r.method == "PATCH" && r.path.ends_with("/card"))
        .collect();
    assert_eq!(probes.len(), 1, "one instance card probe at connect");
    for request in &requests {
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case(lean_ctx_protocol::hive::HEADER_PROTOCOL) {
                assert!(
                    !value.trim_start().starts_with('2'),
                    "{} {} sent Hive-Protocol {value}",
                    request.method,
                    request.path
                );
            }
        }
    }
    let store = machine.store();
    assert!(!lean_ctx::core::hive::bus::relay_v2::speaks_v2(&store));
    let recorded: serde_json::Value = serde_json::from_slice(
        &std::fs::read(store.config_dir().join("bus_protocol.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(recorded, serde_json::json!({"bus": 1}));
    assert!(machine.status_json().connected);
    // Task Compute (C0 2.x only) is "not available" here, and nothing asks.
    let before = fake.requests().len();
    for args in [
        &["compute", "status", "cj_0b4f9a7e3c2d4e1f8a6b5c4d3e2f1a09"][..],
        &[
            "compute",
            "submit",
            "--type",
            "tests",
            "--image",
            "rust-1.99",
            "--",
            "true",
        ][..],
    ] {
        let out = machine.run(args);
        assert!(!out.status.success(), "{args:?}: {out:?}");
        let stderr = text(&out.stderr);
        assert!(
            stderr.contains("not_available: Task Compute is not available on this Hive"),
            "{args:?}: {stderr}"
        );
    }
    let compute_requests = fake.requests()[before..]
        .iter()
        .filter(|r| r.path.contains("/compute/"))
        .count();
    assert_eq!(compute_requests, 0, "a 1.x Hive is never asked");
}

/// Another root key than the one the fake signs with (32 bytes of 7).
const OTHER_ROOT: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

/// Hive task #1: Hive enrolled the key, the engine then refused the
/// answer (a key set not signed by its root). The spent key goes, the error
/// says how to go on, and the next connect works with a fresh key.
#[test]
fn a_connect_refused_after_enrolment_drops_its_key_and_the_next_one_works() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    let config = machine.home.path().join("config/config.toml");
    let right = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("[hive]\nroot_key = \"{OTHER_ROOT}\"\n")).unwrap();

    let refused = machine.connect(&fake);
    assert!(!refused.status.success(), "{refused:?}");
    let stderr = text(&refused.stderr);
    assert!(stderr.contains("not signed by"), "{stderr}");
    // Hive task lean-ctx #2: which root, and how to use the right one.
    assert!(
        stderr.contains("development root key from `[hive] root_key`")
            && stderr.contains("remove `root_key` from `[hive]`")
            && stderr.contains(&fake.url),
        "the error names the configured root and the fix: {stderr}"
    );
    assert!(
        stderr.contains("unused instance key was removed")
            && stderr.contains("lean-ctx hive connect"),
        "the error names the way on: {stderr}"
    );
    let keys = machine.home.path().join("config/hive");
    assert!(!keys.join("instance.key").exists() && !keys.join("kx.key").exists());
    assert!(!machine.status_json().connected);

    std::fs::write(&config, right).unwrap();
    let output = machine.connect(&fake);
    assert!(output.status.success(), "{output:?}");
    assert!(machine.status_json().connected);
}

/// Hive task #1: `disconnect` on a machine that is not connected but holds
/// a key an unfinished connect left removes it.
#[test]
fn disconnect_removes_a_key_an_unfinished_connect_left() {
    let machine = Machine::new();
    lean_ctx::core::hive::instance_key::InstanceKeys::load_or_create(&machine.store()).unwrap();
    let output = machine.run(&["disconnect"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        text(&output.stdout).contains("Removed the instance key an unfinished connect left"),
        "{output:?}"
    );
    assert!(lean_ctx::core::hive::instance_key::InstanceKeys::load(&machine.store()).is_err());
    let again = machine.run(&["disconnect"]);
    assert_eq!(text(&again.stdout).trim(), "Not connected to Hive.");
}

/// Dogfood 25.10.: disconnect said "removed" while the instance stayed
/// active in Hive. Hive is told first; unreachable, nothing is removed and
/// the disconnect stays pending.
#[test]
fn an_unreachable_hive_keeps_the_engine_and_a_pending_disconnect() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    let connected = machine.connect(&fake);
    assert!(connected.status.success(), "{connected:?}");
    drop(fake);
    let output = machine.run(&["disconnect"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = text(&output.stdout);
    assert!(stdout.contains("Nothing was removed"), "{stdout}");
    assert!(!stdout.contains("Removed"), "{stdout}");
    let store = machine.store();
    assert!(store.instance().unwrap().is_some(), "credentials kept");
    assert!(store.config_dir().join("disconnect-pending.json").exists());
    let status = machine.run(&["status"]);
    assert!(
        text(&status.stdout).contains("Disconnect:   pending"),
        "{status:?}"
    );
}

#[test]
fn plan_and_seat_limits_are_explained_without_retrying() {
    let fake = FakeHive::start_with(FakeOptions {
        seats_limit: 1,
        ..FakeOptions::default()
    });
    let first = Machine::new();
    assert!(first.connect(&fake).status.success());
    let second = Machine::new();
    let output = second.connect(&fake);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("all engine instances of the workspace are in use (1 of 1)"),
        "{output:?}"
    );
    assert!(!second.status_json().connected);

    let no_plan = FakeHive::start_with(FakeOptions {
        plan: None,
        ..FakeOptions::default()
    });
    let output = Machine::new().connect(&no_plan);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("Personal 3 engines, 2 connected at once; Team 5 per seat"),
        "{output:?}"
    );

    let old = FakeHive::start_with(FakeOptions {
        min_engine: Some("9.0.0".into()),
        ..FakeOptions::default()
    });
    let output = Machine::new().connect(&old);
    assert!(
        text(&output.stderr).contains("needs lean-ctx 9.0.0"),
        "{output:?}"
    );
}

/// P7: a Personal slot that is cooling down after a revocation is explained
/// with its end and the web recovery, and `hive status` keeps saying it.
#[test]
fn a_cooling_engine_slot_points_to_the_web_recovery_and_stays_in_status() {
    let reset = (chrono::Utc::now() + chrono::Duration::days(3))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let fake = FakeHive::start_with(FakeOptions {
        seats_limit: 1,
        slot_cooldown_until: Some(reset.clone()),
        ..FakeOptions::default()
    });
    let first = Machine::new();
    assert!(first.connect(&fake).status.success());
    let second = Machine::new();
    let output = second.connect(&fake);
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "this workspace has used all its engine slots (1 of 1); nothing is charged for more; \
             a revoked engine's slot frees at {reset} (7-day cool-down), or for a lost or broken \
             device, recover its slot now on the Hive web (Settings › Engines › Recover, with your \
             second factor; once per 30 days)"
        )),
        "{output:?}"
    );
    assert!(
        text(&output.stdout).contains(&format!("Engines:      {} (Settings › Engines)", fake.url)),
        "{output:?}"
    );
    let status = second.run(&["status"]);
    assert!(
        text(&status.stdout).contains(&format!("Throttled:    connect until {reset}: ")),
        "{status:?}"
    );
    assert!(!second.status_json().connected);
}

#[test]
fn status_is_local_and_the_lease_moves_through_grace_to_lapsed() {
    let fake = FakeHive::start_with(FakeOptions {
        plan: Some(HivePlan::Personal),
        seats_limit: 2,
        ..FakeOptions::default()
    });
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let store = machine.store();
    let config = config_for(&fake);

    let root: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(FakeHive::root_public_key_b64())
        .unwrap()
        .try_into()
        .unwrap();
    let mut session = Session::load(store.clone(), root).unwrap();
    let first = session.lease.lease.lease_id.clone();
    session.renew().unwrap();
    assert_ne!(
        session.lease.lease.lease_id, first,
        "renew issues a new signed lease"
    );

    let expires = chrono::DateTime::parse_from_rfc3339(session.lease.lease.expires_at.as_str())
        .unwrap()
        .with_timezone(&chrono::Utc);
    let local = |at: chrono::DateTime<chrono::Utc>| {
        at - chrono::Duration::seconds(session.lease.offset_seconds)
    };
    let in_grace = report(&store, &config, local(expires + chrono::Duration::hours(1)));
    assert_eq!(in_grace.lease_state, Some(LeaseState::Grace));
    assert!(in_grace.cli_status().connected);
    let lapsed = report(&store, &config, local(expires + chrono::Duration::days(8)));
    assert_eq!(lapsed.lease_state, Some(LeaseState::Lapsed));
    assert!(!lapsed.cli_status().connected);

    // Without the service the status still answers from local state.
    drop(fake);
    assert!(machine.status_json().connected);

    // A tampered lease is never trusted.
    let lease_file = store.config_dir().join("lease.json");
    let tampered = std::fs::read_to_string(&lease_file)
        .unwrap()
        .replace("\"plan\": \"personal\"", "\"plan\": \"enterprise\"");
    std::fs::write(&lease_file, tampered).unwrap();
    let status = machine.status_json();
    assert!(!status.connected);
    assert!(text(&machine.run(&["status"]).stdout).contains("Problem:"));
}

#[test]
fn link_writes_the_repo_binding_and_help_has_no_side_effects() {
    let machine = Machine::new();
    let output = machine.run(&["--help"]);
    assert!(output.status.success());
    assert!(text(&output.stdout).contains("lean-ctx hive connect"));
    assert!(!machine.home.path().join("config/hive").exists());

    let output = machine.run(&["link", "acme/payments"]);
    assert!(output.status.success(), "{output:?}");
    let link: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(machine.home.path().join(".lean-ctx-hive.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        link["repo"],
        serde_json::json!({"ns": "acme", "name": "payments"})
    );
    assert!(machine.home.path().join(".lean-ctx-id").exists());

    let output = machine.run(&["link", "no-slash"]);
    assert_eq!(output.status.code(), Some(1));
}

/// `hive <sub> --help` prints that subcommand's usage, wherever the flag
/// stands; `hive tasks help` too.
#[test]
fn hive_subcommand_help_shows_its_own_usage() {
    let m = Machine::new();
    for (args, expected) in [
        (
            &["tasks", "--help"][..],
            "lean-ctx hive tasks claim <number> --agent",
        ),
        (
            &["tasks", "help"][..],
            "lean-ctx hive tasks claim <number> --agent",
        ),
        (
            &["tasks", "claim", "--help"][..],
            "lean-ctx hive tasks claim <number> --agent",
        ),
        (
            &["swarm", "--help"][..],
            "swarm       Swarm subtasks within",
        ),
        (&["worker", "--help"][..], "worker      Worker mode"),
        (
            &["compute", "--help"][..],
            "lean-ctx hive compute result <job id> [--out <dir>]",
        ),
        (
            &["compute"][..],
            "lean-ctx hive compute result <job id> [--out <dir>]",
        ),
        (
            &["compute", "submit", "--help"][..],
            "lean-ctx hive compute submit --type",
        ),
    ] {
        let out = m.run(args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        let stdout = text(&out.stdout);
        assert!(stdout.contains(expected), "{args:?}: {stdout}");
        assert!(
            !stdout.contains("Full command reference"),
            "{args:?}: {stdout}"
        );
    }
}

/// A git checkout with `README.md` committed at `HEAD` (the reference
/// [`remember`] gives every fact, so it passes the capture gate) and
/// `origin` as its remote.
fn git_project(machine: &Machine, name: &str, origin: &str) -> std::path::PathBuf {
    let project = machine.home.path().join(name);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("README.md"), format!("# {name}\n")).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "README.md"]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["remote", "add", "origin", origin]);
    project
}

fn remember(machine: &Machine, project: &Path, value: &str, category: &str, key: &str) {
    let output = machine.run_in(
        project,
        &[
            "knowledge",
            "remember",
            value,
            "--category",
            category,
            "--key",
            key,
            "--ref",
            "file:README.md",
        ],
    );
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn session_learnings_stream_out_as_screened_drafts_and_revisions_stream_back() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = git_project(
        &machine,
        "payments",
        "https://ci-bot:ghp_tokentoken@github.com/acme/payments.git",
    );

    let canary = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";
    remember(
        &machine,
        &project,
        "Billing uses Postgres 16 with logical replication.",
        "architecture-decision",
        "billing-db",
    );
    remember(
        &machine,
        &project,
        &format!("CI token is {canary}."),
        "architecture-decision",
        "ci-token",
    );
    remember(
        &machine,
        &project,
        "Escalations go to jane.doe@example.com.",
        "architecture-decision",
        "escalation",
    );

    let output = machine.run_in(&project, &["hive", "sync"]);
    assert!(output.status.success(), "{output:?}");
    assert!(text(&output.stdout).contains("Sent draft #1"), "{output:?}");

    let requests = fake.requests();
    let home = machine.home.path().to_string_lossy().into_owned();
    for request in &requests {
        for leak in [
            canary,
            "ghp_tokentoken",
            "ci-bot",
            "jane.doe@example.com",
            home.as_str(),
        ] {
            assert!(
                !request.body.contains(leak),
                "{leak} left the machine in {}",
                request.path
            );
        }
    }
    for request in &requests {
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "hive-names" && value == "vault"),
            "{} asked for the old names",
            request.path
        );
    }
    let resolve = requests
        .iter()
        .find(|r| r.path == "/v1/vaults/resolve")
        .unwrap();
    let resolve: serde_json::Value = serde_json::from_str(&resolve.body).unwrap();
    assert_eq!(resolve["label"], "payments");
    let draft_request = requests
        .iter()
        .find(|r| r.path.ends_with("/proposals") && r.method == "POST")
        .unwrap();
    let project_id = draft_request.path.split('/').nth(3).unwrap().to_string();
    let drafts = fake.proposals(&project_id);
    assert_eq!(drafts.len(), 1);
    let ops = drafts[0]["context_patch"]["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1, "only the clean fact is proposed: {ops:?}");
    assert_eq!(ops[0]["fact"]["kind"], "decision");
    assert!(
        drafts[0]["engine_screening"]["items_dropped"]
            .as_u64()
            .unwrap()
            >= 1
    );

    // A second sync has nothing new to say.
    let again = machine.run_in(&project, &["hive", "sync"]);
    assert!(text(&again.stdout).contains("Nothing new"), "{again:?}");
    assert_eq!(fake.proposals(&project_id).len(), 1);

    // A reviewer merges a revision in Hive; the next sync brings it to the agent.
    let revision = fake.publish_revision(
        &project_id,
        serde_json::json!({
            "format": "ctxpkg.research-context.v1",
            "facts": [{"id": "billing.replication", "kind": "decision",
                       "statement": "Billing replicates to the analytics cluster nightly."}],
            "relations": [],
        }),
    );
    let output = machine.run_in(&project, &["hive", "sync"]);
    assert!(
        text(&output.stdout).contains(&format!("revision {revision} (1 facts)")),
        "{output:?}"
    );
    let recalled = machine.run_in(&project, &["knowledge", "recall", "--json", "replicates"]);
    assert!(
        text(&recalled.stdout).contains("Billing replicates to the analytics cluster nightly."),
        "{recalled:?}"
    );

    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Drafts:       0 queued, 0 refused"),
        "{status}"
    );
}

/// An org policy signed now with the fixture signer seed, as Hive sends it.
fn signed_org_policy(version: u64, pack: &str) -> serde_json::Value {
    signed_org_policy_requiring(version, pack, Vec::new())
}

/// [`signed_org_policy`] that also requires filter packs (Hive contract 1.8).
fn signed_org_policy_requiring(
    version: u64,
    pack: &str,
    required: Vec<lean_ctx_protocol::hive::policy::RequiredFilterPackV1>,
) -> serde_json::Value {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::Signer as _;
    use sha2::Digest as _;
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x4f; 32]);
    let public = key.verifying_key().to_bytes();
    let hex = |b: &[u8]| hex::encode(b);
    let at = |d: chrono::Duration| {
        (chrono::Utc::now() + d).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    // Declaration order of OrgPolicyStatementV2 is the signed order.
    #[derive(serde::Serialize)]
    struct Statement {
        schema: &'static str,
        organization_id: &'static str,
        key_id: String,
        policy_version: u64,
        issued_at: String,
        not_before: String,
        not_after: String,
        grace_seconds: u64,
        pack_digest: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        required_filter_packs: Vec<lean_ctx_protocol::hive::policy::RequiredFilterPackV1>,
    }
    let statement = Statement {
        schema: "leanctx.org-policy/v2",
        organization_id: "org_acme",
        key_id: format!("key:ed25519-{}", &hex(&sha2::Sha256::digest(public))[..32]),
        policy_version: version,
        issued_at: at(chrono::Duration::minutes(-5)),
        not_before: at(chrono::Duration::minutes(-5)),
        not_after: at(chrono::Duration::days(30)),
        grace_seconds: 86_400,
        pack_digest: format!("sha256:{}", hex(&sha2::Sha256::digest(pack.as_bytes()))),
        required_filter_packs: required,
    };
    let mut signing = b"leanctx.org-policy.v2\n".to_vec();
    signing.extend(serde_json::to_vec(&statement).unwrap());
    serde_json::json!({
        "statement": statement,
        "public_key": URL_SAFE_NO_PAD.encode(public),
        "signature": URL_SAFE_NO_PAD.encode(key.sign(&signing).to_bytes()),
        "pack_toml": pack,
    })
}

#[test]
fn org_policy_bundles_are_verified_against_the_lease_and_a_bad_one_never_replaces_the_good_one() {
    let fake = FakeHive::start_with(FakeOptions {
        policy_signer_keys: vec![lean_ctx_protocol::hive::lease::PolicySignerKeyV1 {
            org: "org_acme".into(),
            ed25519_pub: base64::engine::general_purpose::STANDARD.encode(
                ed25519_dalek::SigningKey::from_bytes(&[0x4f; 32])
                    .verifying_key()
                    .as_bytes(),
            ),
        }],
        ..FakeOptions::default()
    });
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let pack = "name = \"acme-floor\"\nversion = \"1.0.0\"\ndescription = \"floor\"\n\n[context]\ndeny_tools = [\"ctx_url_read\"]\n";
    let bundle = |version: u64, policy: serde_json::Value| {
        serde_json::json!({
            "schema": "leanctx.hive.policy-bundle/v1", "policy_version": version,
            "org_policy": policy, "unpin": null, "filter_packs": [],
        })
    };
    fake.set_policy_bundle(bundle(1, signed_org_policy(1, pack)));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stdout).contains("Policy bundle verified (org policy v1"),
        "{output:?}"
    );
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Company policy: v1 received and verified"),
        "{status}"
    );

    // Version 2 with a pack swapped after signing: refused, v1 stays.
    let mut tampered = signed_org_policy(2, pack);
    tampered["pack_toml"] = serde_json::Value::from(format!("{pack}\n[context]\n"));
    fake.set_policy_bundle(bundle(2, tampered));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stderr)
            .contains("policy bundle refused: the policy pack is not the one the statement names"),
        "{output:?}"
    );
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Company policy: v1 received and verified"),
        "{status}"
    );
    assert!(status.contains("Policy refused:"), "{status}");

    // A downgrade is refused too.
    fake.set_policy_bundle(bundle(1, signed_org_policy(1, "name = \"weaker\"\n")));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stderr).contains("policy bundle refused"),
        "{output:?}"
    );
}

/// HIVE-RT-D5: Hive cannot drop a pack the org's signed policy requires;
/// the engine refuses that bundle, keeps the last verified one, says so in
/// `hive status`, and local work goes on.
#[test]
fn a_bundle_without_a_pack_the_signed_policy_requires_is_refused() {
    let fake = FakeHive::start_with(FakeOptions {
        policy_signer_keys: vec![lean_ctx_protocol::hive::lease::PolicySignerKeyV1 {
            org: "org_acme".into(),
            ed25519_pub: base64::engine::general_purpose::STANDARD.encode(
                ed25519_dalek::SigningKey::from_bytes(&[0x4f; 32])
                    .verifying_key()
                    .as_bytes(),
            ),
        }],
        ..FakeOptions::default()
    });
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let pack = "name = \"acme-floor\"\nversion = \"1.0.0\"\ndescription = \"floor\"\n\n[context]\ndeny_tools = [\"ctx_url_read\"]\n";
    let filters = signed_filter_pack("acme-codenames", 1, "project-falcon", "block");
    // The contract type: its field order is the signed order (a JSON object
    // would sort the keys and break the signature).
    let required = vec![lean_ctx_protocol::hive::policy::RequiredFilterPackV1 {
        pack_id: "acme-codenames".into(),
        version: 1,
        sha256: filters["sha256"].as_str().unwrap().into(),
    }];
    let policy = signed_org_policy_requiring(1, pack, required);
    let bundle = |packs: serde_json::Value| {
        serde_json::json!({
            "schema": "leanctx.hive.policy-bundle/v1", "policy_version": 1,
            "org_policy": policy, "unpin": null, "filter_packs": packs,
        })
    };
    fake.set_policy_bundle(bundle(serde_json::json!([filters])));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stdout).contains("Policy bundle verified (org policy v1"),
        "{output:?}"
    );
    // The status names the required pack and whether it is installed as signed.
    let json: serde_json::Value =
        serde_json::from_str(&text(&machine.run(&["status", "--json"]).stdout)).unwrap();
    assert_eq!(
        json["policy"]["required_filter_packs"],
        serde_json::json!([{"pack_id": "acme-codenames", "version": 1, "installed": true}]),
        "{json}"
    );
    assert!(
        text(&machine.run(&["status"]).stdout)
            .contains("Required:     acme-codenames v1 (installed)")
    );

    // The same signed policy, the pack left out: refused, the pack stays.
    fake.set_policy_bundle(bundle(serde_json::json!([])));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stderr)
            .contains("policy bundle refused: required filter pack acme-codenames is missing"),
        "{output:?}"
    );
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Filter packs: acme-codenames v1"),
        "{status}"
    );
    assert!(
        status.contains("Policy refused: required filter pack acme-codenames is missing"),
        "{status}"
    );
}

/// A filter pack signed with the fake's Hive key, as Hive lists it.
fn signed_filter_pack(
    pack_id: &str,
    version: u64,
    pattern: &str,
    action: &str,
) -> serde_json::Value {
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::Signer as _;
    use sha2::Digest as _;
    let statement = lean_ctx_protocol::hive::canonical_json(&serde_json::json!({
        "schema": "leanctx.filter-pack/v1", "pack_id": pack_id, "version": version,
        "patterns": [{"id": "codename", "kind": "regex", "pattern": pattern, "action": action}],
    }))
    .unwrap();
    let mut signing = b"leanctx.filter-pack.v1\n".to_vec();
    signing.extend_from_slice(&statement);
    let signature = ed25519_dalek::SigningKey::from_bytes(&[0x53; 32]).sign(&signing);
    serde_json::json!({
        "pack_id": pack_id, "version": version,
        "sha256": hex::encode(sha2::Sha256::digest(&statement)),
        "envelope": {"statement_b64": STANDARD.encode(&statement), "kid": "hive-2026-10",
                     "signature": STANDARD.encode(signature.to_bytes())},
    })
}

#[test]
fn org_filter_packs_keep_matching_learnings_on_the_machine() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    fake.set_policy_bundle(serde_json::json!({
        "schema": "leanctx.hive.policy-bundle/v1", "policy_version": 0,
        "org_policy": null, "unpin": null,
        "filter_packs": [signed_filter_pack("acme-codenames", 1, "project-falcon", "block")],
    }));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stdout).contains("1 filter packs"),
        "{output:?}"
    );
    assert!(text(&machine.run(&["status"]).stdout).contains("Filter packs: acme-codenames v1"));

    let project = git_project(&machine, "falcon", "git@github.com:acme/falcon.git");
    remember(
        &machine,
        &project,
        "Launch of project-falcon is in March.",
        "decision",
        "launch",
    );
    remember(
        &machine,
        &project,
        "Deploys run on Fridays after review.",
        "deploy",
        "day",
    );
    let output = machine.run_in(&project, &["hive", "sync"]);
    assert!(text(&output.stdout).contains("Sent draft"), "{output:?}");
    for request in fake.requests() {
        assert!(
            !request.body.contains("project-falcon"),
            "the codename left the machine"
        );
    }

    // A pack whose signature was made by another key is refused; the good one stays.
    let mut forged = signed_filter_pack("acme-codenames", 2, "x", "block");
    forged["envelope"]["signature"] =
        signed_filter_pack("other", 1, "y", "block")["envelope"]["signature"].clone();
    fake.set_policy_bundle(serde_json::json!({
        "schema": "leanctx.hive.policy-bundle/v1", "policy_version": 0,
        "org_policy": null, "unpin": null, "filter_packs": [forged],
    }));
    let output = machine.run(&["sync"]);
    assert!(
        text(&output.stderr).contains("signature does not verify"),
        "{output:?}"
    );
    assert!(text(&machine.run(&["status"]).stdout).contains("Filter packs: acme-codenames v1"));
}

#[test]
fn drafts_wait_in_the_outbox_while_hive_is_offline() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = git_project(&machine, "offline", "git@github.com:acme/offline.git");
    remember(
        &machine,
        &project,
        "Releases are cut on Tuesdays.",
        "release",
        "cadence",
    );
    drop(fake);

    let output = machine.run_in(&project, &["hive", "sync"]);
    assert!(
        output.status.success(),
        "a failed sync is no error: {output:?}"
    );
    let status = text(&machine.run(&["status"]).stdout);
    assert!(status.contains("Drafts:       1 queued"), "{status}");
    assert!(status.contains("Captured 1 fact"), "{status}");
}

#[test]
fn metrics_follow_the_engine_lease_and_organization_switches() {
    use lean_ctx_protocol::hive::lease::HiveOrgSettingsV1;
    // The organization switched metrics off; the signed lease says so.
    let fake = FakeHive::start_with(FakeOptions {
        org_settings: Some(HiveOrgSettingsV1 {
            metrics_upload: false,
            receipts_upload: false,
        }),
        ..FakeOptions::default()
    });
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Metrics:      not sent: switched off by the workspace"),
        "{status}"
    );
    let sync = machine.run(&["sync"]);
    assert!(sync.status.success(), "{sync:?}");
    assert!(
        text(&sync.stdout).contains("Metrics not sent: switched off by the workspace."),
        "{sync:?}"
    );
    assert!(
        fake.requests().iter().all(|r| r.path != "/v1/metrics"),
        "nothing was sent"
    );

    // An organization that allows it, and an engine that does not.
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Metrics:      not sent: replaced by hourly engine metrics"),
        "the daily upload is retired: {status}"
    );
    assert!(
        status.contains("Receipts:     not sent: the lease does not include receipts.upload"),
        "a Team lease has no receipts right: {status}"
    );
    let config = machine.home.path().join("config/config.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str("metrics_upload = false\n");
    std::fs::write(&config, toml).unwrap();
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Metrics:      not sent: switched off on this engine"),
        "{status}"
    );
}

/// Q1 (C0 1.9 §7): a confirmation brings a sealed proposal to its
/// threshold; Hive cannot read it, so it waits `ready` until an engine
/// that may merge does so on its next sync. The CLI confirms nothing.
#[test]
fn confirmed_sealed_proposals_are_merged_by_an_engine_and_the_cli_confirms_nothing() {
    use lean_ctx_protocol::hive::lease::HiveRight;
    let mut rights = FakeOptions::default().rights;
    rights.extend([HiveRight::Sealed, HiveRight::KnowledgeMerge]);
    let fake = FakeHive::start_with(FakeOptions {
        rights,
        seal_new_projects: true,
        ..FakeOptions::default()
    });
    let origin = "https://github.com/acme/vault.git";
    let a = Machine::new();
    assert!(a.connect(&fake).status.success());
    let a_id = a.status_json().instance.unwrap().id;
    let a_project = git_project(&a, "vault", origin);
    assert!(a.run_in(&a_project, &["hive", "sync"]).status.success());
    let project_id = fake.project_ids().into_iter().next().unwrap();
    fake.approve_sealed(&project_id, &a_id);
    remember(
        &a,
        &a_project,
        "Ledgers close at midnight UTC.",
        "architecture-decision",
        "close",
    );
    assert!(text(&a.run_in(&a_project, &["hive", "sync"]).stdout).contains("Sent draft #1"));
    fake.set_auto_merge(&project_id, 1);

    let b = Machine::new();
    assert!(b.connect(&fake).status.success());
    let b_id = b.status_json().instance.unwrap().id;
    fake.approve_sealed(&project_id, &b_id);
    assert!(a.run(&["grant", "--yes"]).status.success());
    let b_project = git_project(&b, "vault", origin);
    assert!(b.run_in(&b_project, &["hive", "sync"]).status.success());
    let repo = format!("acme/p-{}", &project_id[..8]);

    // The CLI confirms nothing: people review on the web, agents through
    // their own session.
    let cli = b.run(&["review", "confirm", &repo, "1"]);
    assert_eq!(cli.status.code(), Some(1));
    assert!(text(&cli.stderr).contains("on the web"), "{cli:?}");
    assert!(
        fake.requests()
            .iter()
            .all(|r| !r.path.ends_with("/confirmations")),
        "nothing was confirmed"
    );

    // A person confirms on the web: the sealed proposal is ready, and the
    // next sync of an engine that may merge merges it.
    fake.confirm_as_person(&project_id, 1);
    assert_eq!(
        fake.proposal_state(&project_id, 1),
        Some(serde_json::json!("ready"))
    );
    let listing = text(&b.run(&["review"]).stdout);
    assert!(
        listing.contains("#1 sealed"),
        "ready proposals are listed: {listing}"
    );
    let sync = a.run_in(&a_project, &["hive", "sync"]);
    assert!(
        text(&sync.stdout).contains("Merged the confirmed sealed proposals of acme/"),
        "{sync:?}"
    );
    assert_eq!(
        fake.proposal_state(&project_id, 1),
        Some(serde_json::json!("merged"))
    );
}

/// X2 (C0 1.9 §3): a threshold merge in a quarantining plan never reaches
/// an agent on any machine; the engine keeps merging on top of it, and a
/// rollback to the released head undoes it, released at once.
#[test]
fn a_quarantined_threshold_merge_never_reaches_an_agent_and_rolls_back_to_the_released_head() {
    use lean_ctx_protocol::hive::knowledge::RevisionState;
    use lean_ctx_protocol::hive::lease::HiveRight;
    let mut rights = FakeOptions::default().rights;
    rights.extend([HiveRight::Sealed, HiveRight::KnowledgeMerge]);
    let fake = FakeHive::start_with(FakeOptions {
        rights,
        seal_new_projects: true,
        ..FakeOptions::default()
    });
    let origin = "https://github.com/acme/vault.git";
    let a = Machine::new();
    assert!(a.connect(&fake).status.success());
    let a_id = a.status_json().instance.unwrap().id;
    let a_project = git_project(&a, "vault", origin);
    assert!(a.run_in(&a_project, &["hive", "sync"]).status.success());
    let project_id = fake.project_ids().into_iter().next().unwrap();
    fake.approve_sealed(&project_id, &a_id);
    let repo = format!("acme/p-{}", &project_id[..8]);
    remember(
        &a,
        &a_project,
        "Ledgers close at midnight UTC.",
        "architecture-decision",
        "close",
    );
    assert!(text(&a.run_in(&a_project, &["hive", "sync"]).stdout).contains("Sent draft #1"));
    let merged = a.run(&["review", "merge", &repo, "1"]);
    assert!(
        text(&merged.stdout).contains("revision 1 (sealed)"),
        "{merged:?}"
    );

    let b = Machine::new();
    assert!(b.connect(&fake).status.success());
    let b_id = b.status_json().instance.unwrap().id;
    fake.approve_sealed(&project_id, &b_id);
    assert!(a.run(&["grant", "--yes"]).status.success());
    let b_project = git_project(&b, "vault", origin);
    let recall = |what: &str| {
        assert!(b.run_in(&b_project, &["hive", "sync"]).status.success());
        text(
            &b.run_in(&b_project, &["knowledge", "recall", "--json", what])
                .stdout,
        )
    };
    assert!(recall("midnight").contains("Ledgers close at midnight UTC."));

    fake.set_auto_merge(&project_id, 1);
    fake.set_quarantine(&project_id, true);
    remember(
        &a,
        &a_project,
        "Refunds settle within two days.",
        "architecture-decision",
        "refunds",
    );
    assert!(text(&a.run_in(&a_project, &["hive", "sync"]).stdout).contains("Sent draft #2"));
    fake.confirm_as_person(&project_id, 2);
    let sync = a.run_in(&a_project, &["hive", "sync"]);
    assert!(
        text(&sync.stdout).contains("Merged the confirmed sealed proposals of acme/"),
        "{sync:?}"
    );
    assert_eq!(
        fake.revision_state(&project_id, 2),
        Some(RevisionState::Quarantined)
    );
    assert!(
        !recall("refunds").contains("Refunds settle"),
        "a quarantined revision reaches no agent"
    );
    assert!(recall("midnight").contains("Ledgers close at midnight UTC."));

    // Only a released revision is a rollback target; the released head
    // itself is one while a quarantined revision waits after it.
    let refused = a.run(&["review", "rollback", &repo, "2"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(
        text(&refused.stderr).contains("not an earlier released revision"),
        "{refused:?}"
    );
    let rolled = a.run(&["review", "rollback", &repo, "1"]);
    assert!(rolled.status.success(), "{rolled:?}");
    assert_eq!(fake.revision(&project_id, 3).unwrap().1, Some(1));
    assert_eq!(
        fake.revision_state(&project_id, 2),
        Some(RevisionState::RolledBack)
    );
    assert_eq!(
        fake.revision_state(&project_id, 3),
        Some(RevisionState::Released)
    );
    assert!(!recall("refunds").contains("Refunds settle"));
    assert!(recall("midnight").contains("Ledgers close at midnight UTC."));
}

#[test]
fn a_sealed_repo_publishes_its_code_graph_sealed_without_a_readable_path() {
    use lean_ctx_protocol::hive::lease::HiveRight;
    let mut rights = FakeOptions::default().rights;
    rights.push(HiveRight::Sealed);
    let fake = FakeHive::start_with(FakeOptions {
        rights,
        seal_new_projects: true,
        ..FakeOptions::default()
    });
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let instance_id = machine.status_json().instance.unwrap().id;
    let project = git_project(&machine, "ledger", "https://github.com/acme/ledger.git");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("src/settlement_ledger.rs"),
        "pub fn reconcile_quarterly_settlements() -> u32 {\n    7\n}\n",
    )
    .unwrap();
    assert!(machine.run_in(&project, &["hive", "sync"]).status.success());
    let project_id = fake.project_ids().into_iter().next().unwrap();
    fake.approve_sealed(&project_id, &instance_id);
    // The repo's first key comes with the first sealed content.
    remember(
        &machine,
        &project,
        "Settlements reconcile quarterly.",
        "architecture-decision",
        "settlement",
    );
    assert!(machine.run_in(&project, &["hive", "sync"]).status.success());

    let published = machine.run_in(&project, &["hive", "graph", "publish"]);
    assert!(published.status.success(), "{published:?}");
    assert!(
        text(&published.stdout).contains("sent sealed"),
        "{published:?}"
    );
    // The file and its symbol are in the graph, only not readable.
    assert!(
        !text(&published.stdout).contains(": 0 nodes"),
        "{published:?}"
    );
    let puts: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|r| r.method == "PUT" && r.path.ends_with("/code-graph"))
        .collect();
    assert_eq!(puts.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&puts[0].body).unwrap();
    assert!(body["graph"].is_null(), "{body}");
    assert!(body["sealed"]["ciphertext"].is_string(), "{body}");
    // No path, file or symbol name of the checkout in clear (each needle
    // has a character base64 ciphertext never contains).
    for plaintext in [
        "settlement_ledger",
        "reconcile_quarterly_settlements",
        "code:",
    ] {
        assert!(!puts[0].body.contains(plaintext), "{plaintext} in {body}");
    }
}

#[test]
fn a_sealed_repo_sends_nothing_readable_and_shares_its_key_after_a_fingerprint_check() {
    use lean_ctx_protocol::hive::lease::HiveRight;
    let mut rights = FakeOptions::default().rights;
    rights.push(HiveRight::Sealed);
    // Members review sealed proposals in their engines, so they merge there.
    rights.push(HiveRight::KnowledgeMerge);
    let fake = FakeHive::start_with(FakeOptions {
        rights,
        seal_new_projects: true,
        ..FakeOptions::default()
    });
    let origin = "https://github.com/acme/vault.git";

    // A: the first engine of the sealed vault creates its key.
    let a = Machine::new();
    assert!(a.connect(&fake).status.success());
    let a_id = a.status_json().instance.unwrap().id;
    let a_project = git_project(&a, "vault", origin);
    assert!(a.run_in(&a_project, &["hive", "sync"]).status.success());
    let project_id = fake.project_ids().into_iter().next().unwrap();
    fake.approve_sealed(&project_id, &a_id);
    let fact = "Billing replicates to the analytics cluster nightly.";
    remember(&a, &a_project, fact, "architecture-decision", "replication");
    let sync = a.run_in(&a_project, &["hive", "sync"]);
    assert!(text(&sync.stdout).contains("Sent draft #1"), "{sync:?}");
    assert_eq!(fake.sealed_grants(&project_id), vec![(a_id.clone(), 1)]);

    // Nothing readable left the machine: not the fact, not a summary.
    for request in fake.requests() {
        for plaintext in ["Billing", "replicates", "analytics", "Captured"] {
            assert!(
                !request.body.contains(plaintext),
                "{plaintext} left the machine in {}",
                request.path
            );
        }
    }
    let drafts = fake.proposals(&project_id);
    assert_eq!(drafts.len(), 1);
    assert!(drafts[0].get("summary").is_none(), "{:?}", drafts[0]);
    assert!(drafts[0]["context_patch"]["sealed"]["ciphertext"].is_string());

    // B: approved by a maintainer, granted by A after the fingerprint check.
    let b = Machine::new();
    assert!(b.connect(&fake).status.success());
    let b_id = b.status_json().instance.unwrap().id;
    let b_print = text(&b.run(&["status"]).stdout)
        .lines()
        .find_map(|line| line.strip_prefix("Key print:    "))
        .map(|rest| {
            rest.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .expect("own fingerprint in status");
    fake.approve_sealed(&project_id, &b_id);
    let refused = a.run(&["grant"]);
    assert!(
        text(&refused.stdout).contains(&b_print),
        "the fingerprint is shown: {refused:?}"
    );
    assert!(text(&refused.stdout).contains("not granted"), "{refused:?}");
    let granted = a.run(&["grant", "--yes"]);
    assert!(text(&granted.stdout).contains("granted."), "{granted:?}");
    assert!(fake.sealed_grants(&project_id).contains(&(b_id.clone(), 1)));

    // B seals its own draft with the granted key.
    let b_project = git_project(&b, "vault", origin);
    remember(
        &b,
        &b_project,
        "Rotate the vault key yearly.",
        "architecture-decision",
        "rotation",
    );
    let sync = b.run_in(&b_project, &["hive", "sync"]);
    assert!(text(&sync.stdout).contains("Sent draft #2"), "{sync:?}");
    assert!(
        fake.requests()
            .iter()
            .all(|r| !r.body.contains("Rotate the vault")),
        "B's fact stayed sealed too"
    );

    // A reviews both proposals in its engine and merges them, sealed.
    let listing = text(&a.run(&["review"]).stdout);
    assert!(listing.contains("#1 sealed"), "{listing}");
    assert!(listing.contains("#2 sealed"), "{listing}");
    let repo = format!("acme/p-{}", &project_id[..8]);
    let merged = a.run(&["review", "merge", &repo, "1", "2"]);
    assert!(
        text(&merged.stdout).contains("revision 1 (sealed)"),
        "{merged:?}"
    );
    assert_eq!(fake.head_epoch(&project_id), Some(1));
    for request in fake.requests() {
        for plaintext in ["Billing replicates", "Rotate the vault"] {
            assert!(
                !request.body.contains(plaintext),
                "{plaintext} left in {}",
                request.path
            );
        }
    }
    // B pulls the sealed revision and opens it with its granted key.
    assert!(b.run_in(&b_project, &["hive", "sync"]).status.success());
    let recalled = b.run_in(&b_project, &["knowledge", "recall", "--json", "replicates"]);
    assert!(text(&recalled.stdout).contains(fact), "{recalled:?}");

    // B is removed: A's next sealing moves the repo to epoch 2, re-seals
    // the head, and B gets no key for it.
    fake.revoke_sealed(&project_id, &b_id);
    remember(
        &a,
        &a_project,
        "Keys rotate on every member removal.",
        "architecture-decision",
        "removal",
    );
    let sync = a.run_in(&a_project, &["hive", "sync"]);
    assert!(text(&sync.stdout).contains("Sent draft #3"), "{sync:?}");
    let grants = fake.sealed_grants(&project_id);
    assert!(grants.contains(&(a_id, 2)), "{grants:?}");
    assert!(!grants.contains(&(b_id, 2)), "{grants:?}");
    assert_eq!(
        fake.head_epoch(&project_id),
        Some(2),
        "the head is re-sealed"
    );

    // A merges #3, then rolls back to revision 1 (epoch 1): the restored
    // document is sealed again under epoch 2 for the new number; revision
    // 1's ciphertext is never sent as it is.
    let merged = a.run(&["review", "merge", &repo, "3"]);
    assert!(
        text(&merged.stdout).contains("revision 3 (sealed)"),
        "{merged:?}"
    );
    let rolled = a.run(&["review", "rollback", &repo, "1"]);
    assert!(
        text(&rolled.stdout)
            .contains("back to the document of revision 1: revision 4 (sealed again)"),
        "{rolled:?}"
    );
    let (old, _) = fake.revision(&project_id, 1).unwrap();
    let (new, rolls_back) = fake.revision(&project_id, 4).unwrap();
    let (old, new) = (old.unwrap(), new.unwrap());
    assert_eq!(rolls_back, Some(1));
    assert_eq!(new["epoch"], 2, "sealed under the current key");
    assert_ne!(new["ciphertext"], old["ciphertext"]);
    let old_ciphertext = old["ciphertext"].as_str().unwrap();
    let rollback_put = fake
        .requests()
        .into_iter()
        .rfind(|r| r.method == "PUT" && r.path.ends_with("/knowledge"))
        .expect("the rollback was written");
    assert!(
        rollback_put.body.contains("\"rolls_back\":1"),
        "{}",
        rollback_put.body
    );
    assert!(
        !rollback_put.body.contains(old_ciphertext),
        "the old ciphertext is never sent as it is"
    );
    let recalled = a.run_in(&a_project, &["knowledge", "recall", "--json", "replicates"]);
    assert!(text(&recalled.stdout).contains(fact), "{recalled:?}");

    // A machine that never got the repo key cannot roll back, and says why.
    let c = Machine::new();
    assert!(c.connect(&fake).status.success());
    let c_project = git_project(&c, "vault", origin);
    assert!(c.run_in(&c_project, &["hive", "sync"]).status.success());
    let refused = c.run(&["review", "rollback", &repo, "1"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("holds no key of the sealed vault"),
        "{refused:?}"
    );
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn the_bus_relay_catches_up_and_acks_deliveries_from_other_instances() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = git_project(&machine, "bus", "git@github.com:acme/bus.git");
    assert!(machine.run_in(&project, &["hive", "sync"]).status.success());
    let project_id = fake.project_ids().pop().expect("project resolved");
    let channel = format!("org:org_acme/vault:{project_id}/general");

    // Missed while offline: replayed from the hello cursors.
    let missed = fake.inject_event(
        &channel,
        "codex-85",
        "status",
        &serde_json::json!({"text": "E4 merged"}),
    );
    let home = machine.home.path().to_path_buf();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(&home)
        .env("LEAN_CTX_HIVE_PREVIEW", "1")
        .env("HOME", &home)
        .env("LEAN_CTX_PROJECT_ROOT", &home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
        .env("LEAN_CTX_STATE_DIR", home.join("state"))
        .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .args(["hive", "bus", "relay", "--for", "30"])
        .spawn()
        .unwrap();
    wait_for("the relay to connect", || !fake.bus_connected().is_empty());
    let acked = |seq: u64| {
        fake.bus_acks()
            .iter()
            .any(|(_, ch, s)| ch == &channel && *s == seq)
    };
    wait_for("the missed event to be acked", || acked(missed));

    // Live: delivered over the open socket.
    let live = fake.inject_event(
        &channel,
        "codex-85",
        "handoff",
        &serde_json::json!({"text": "your turn"}),
    );
    wait_for("the live event to be acked", || acked(live));
    // The relay saves its cursor before it acks, so an ack Hive has seen
    // is already on disk; a kill right now must not lose it.
    let state = home.join("data/hive/bus/state.json");
    let cursor = std::fs::read(&state)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["cursors"][&channel].as_u64());
    assert_eq!(cursor, Some(live), "cursor persisted before the ack");
    relay.kill().ok();
    relay.wait().ok();

    // A restart resumes after the cursor: nothing is acked twice.
    let before = fake.bus_acks().len();
    let output = machine.run(&["bus", "relay", "--for", "3"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fake.bus_acks().len(), before, "no replay after the cursor");
}

/// H2: tasks of a linked project are read through the contract, text from
/// Hive arrives fenced as foreign data (X5), a proposal goes out with the next sync (once, under
/// its idempotency key), and a claim goes out only under the engine's opt-in.
#[test]
fn project_tasks_are_listed_proposed_once_and_claimed_only_under_the_optin() {
    use lean_ctx_protocol::hive::lease::HiveRight;
    let mut rights = FakeOptions::default().rights;
    rights.extend([
        HiveRight::TasksRead,
        HiveRight::TasksWork,
        HiveRight::TasksPropose,
    ]);
    let fake = FakeHive::start_with(FakeOptions {
        rights,
        ..FakeOptions::default()
    });
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = git_project(&machine, "payments", "https://github.com/acme/payments.git");
    assert!(machine.run_in(&project, &["hive", "sync"]).status.success());
    let project_id = fake.project_ids().pop().expect("project resolved");
    let number = fake.add_task(
        &project_id,
        "Explain the retry budget",
        "A fact states the budget and why",
    );

    let list = machine.run_in(&project, &["hive", "tasks", "list", "--state", "open"]);
    assert!(list.status.success(), "{list:?}");
    let listed = text(&list.stdout);
    let fence = listed
        .find("<<foreign-data kind=task_body")
        .expect("data header");
    assert!(
        listed.find("Explain the retry budget").unwrap() > fence,
        "{listed}"
    );
    let shown = text(
        &machine
            .run_in(&project, &["hive", "tasks", "show", &number.to_string()])
            .stdout,
    );
    assert!(
        shown.starts_with(&format!("Hive task #{number} — open (person)")),
        "{shown}"
    );
    assert!(shown.contains("Done when: A fact states the budget and why"));

    let claim = || {
        machine.run_in(
            &project,
            &[
                "hive",
                "tasks",
                "claim",
                &number.to_string(),
                "--agent",
                "claude-a",
            ],
        )
    };
    // No opt-in: the worker gate refuses before anything is requested.
    let refused = claim();
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("worker mode is not enabled for this task"),
        "{refused:?}"
    );
    assert!(
        fake.requests().iter().all(|r| !r.path.ends_with("/claim")),
        "no claim left the machine"
    );
    // Opted in for this repo: the claim goes out under the opt-in.
    let mut optin: serde_json::Value = serde_json::from_str(include_str!(
        "../../crates/lean-ctx-protocol/tests/fixtures/hive/engine_optin.response.json"
    ))
    .unwrap();
    optin["vaults"] = serde_json::json!([project_id]);
    optin["used_today"]["day"] =
        serde_json::json!(chrono::Utc::now().format("%Y-%m-%d").to_string());
    fake.set_optin(optin.clone());
    let granted = claim();
    assert!(granted.status.success(), "{granted:?}");
    assert!(
        text(&granted.stdout).starts_with(&format!("Claimed task #{number} until ")),
        "{granted:?}"
    );
    let claimed = fake
        .requests()
        .into_iter()
        .rfind(|r| r.path.ends_with("/claim"))
        .expect("claim sent");
    let body: serde_json::Value = serde_json::from_str(&claimed.body).unwrap();
    assert_eq!(body["optin_id"], optin["optin_id"]);
    let cache = machine.home.path().join("data/hive/task-claims.json");
    #[cfg(unix)]
    assert_eq!(mode(&cache) & 0o077, 0, "the claim cache is private");
    let lease =
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&cache).unwrap())
            .unwrap()["claims"][0]["claim"]["lease_id"]
            .as_str()
            .unwrap()
            .to_owned();
    assert!(
        !text(&granted.stdout).contains(&lease),
        "the lease id is never printed"
    );

    let propose = |title: &str| {
        machine.run_in(
            &project,
            &[
                "hive",
                "tasks",
                "propose",
                "--title",
                title,
                "--acceptance",
                "The flaky timer test passes 100 runs in a row",
            ],
        )
    };
    assert!(propose("Stabilise the timer test").status.success());
    let sync = machine.run_in(&project, &["hive", "sync"]);
    assert!(
        text(&sync.stdout).contains(&format!("Proposed task #{}", number + 1)),
        "{sync:?}"
    );
    // The same proposal again is the same task (idempotency key).
    assert!(propose("Stabilise the timer test").status.success());
    assert!(machine.run_in(&project, &["hive", "sync"]).status.success());
    let tasks = fake.tasks(&project_id);
    assert_eq!(tasks.len(), 2, "{tasks:?}");
    assert_eq!(tasks[1]["state"], "proposed");
    assert_eq!(tasks[1]["origin"]["kind"], "agent");

    let secret = propose(&format!("Rotate {}", concat!("AKIA", "IOSFODNN7EXAMPLE")));
    assert_eq!(secret.status.code(), Some(1));
    assert!(
        text(&secret.stderr).contains("the title was not sent"),
        "{secret:?}"
    );
}

/// A `lean-ctx mcp` session on stdio, as an agent host runs it.
struct McpSession {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl McpSession {
    fn start(machine: &Machine, dir: &Path) -> Self {
        let home = machine.home.path();
        let mut child = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .current_dir(dir)
            .env("LEAN_CTX_HIVE_PREVIEW", "1")
            .env("HOME", home)
            .env("LEAN_CTX_PROJECT_ROOT", dir)
            .env("LEAN_CTX_DATA_DIR", home.join("data"))
            .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
            .env("LEAN_CTX_STATE_DIR", home.join("state"))
            .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .arg("mcp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut session = Self {
            stdin: child.stdin.take().unwrap(),
            stdout: std::io::BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 0,
        };
        session.request(
            "initialize",
            &serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "hive-suite", "version": "1"}}),
        );
        session
            .write(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        session
    }

    fn write(&mut self, message: &serde_json::Value) {
        use std::io::Write as _;
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: &serde_json::Value) -> serde_json::Value {
        use std::io::BufRead as _;
        self.next_id += 1;
        let id = self.next_id;
        self.write(
            &serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        );
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "mcp closed");
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    }

    fn call(&mut self, tool: &str, arguments: &serde_json::Value) -> String {
        let reply = self.request(
            "tools/call",
            &serde_json::json!({"name": tool, "arguments": arguments}),
        );
        reply["result"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect()
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An agent that registers while the relay is connected is announced with
/// a fresh hello on the same socket and can send at once (Hive, like the
/// fake, refuses frames from agents no hello announced).
#[test]
fn an_agent_registered_after_the_relay_connected_is_announced_and_can_send() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = git_project(&machine, "late", "git@github.com:acme/late.git");
    assert!(machine.run_in(&project, &["hive", "sync"]).status.success());
    let home = machine.home.path().to_path_buf();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(&home)
        .env("LEAN_CTX_HIVE_PREVIEW", "1")
        .env("HOME", &home)
        .env("LEAN_CTX_PROJECT_ROOT", &home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
        .env("LEAN_CTX_STATE_DIR", home.join("state"))
        .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .args(["hive", "bus", "relay", "--for", "60"])
        .spawn()
        .unwrap();
    wait_for("the relay's first hello", || !fake.bus_hellos().is_empty());
    let sessions = fake.bus_sessions();

    let mut host = McpSession::start(&machine, &project);
    let registered = host.call(
        "ctx_agent",
        &serde_json::json!({"action": "register", "agent_type": "claude", "role": "late"}),
    );
    let agent = registered
        .split("Agent registered: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("{registered}"))
        .to_string();
    // D6: only an explicitly addressed post leaves the machine; address it
    // to the vault this checkout is linked to.
    let vaults: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("data/hive/projects.json")).unwrap())
            .unwrap();
    let vault = vaults["projects"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    let address = format!(
        "#{}/{}",
        vault["ns"].as_str().unwrap(),
        vault["name"].as_str().unwrap()
    );
    let posted = host.call(
        "ctx_agent",
        &serde_json::json!({"action": "post", "message": "late agent reports in", "category": "status", "to_agent": address}),
    );
    assert!(posted.contains("Posted"), "{posted}");

    wait_for("a hello announcing the late agent", || {
        fake.bus_hellos()
            .last()
            .is_some_and(|(_, agents)| agents.contains(&agent))
    });
    wait_for("the late agent's message on the bus", || {
        fake.bus_events().iter().any(|event| {
            event["from"] == agent.as_str() && event["body"]["text"] == "late agent reports in"
        })
    });
    assert_eq!(
        fake.bus_sessions(),
        sessions,
        "announced on the open socket, without reconnecting"
    );
    assert!(
        fake.bus_hellos().len() >= 2,
        "a second hello on that socket"
    );
    relay.kill().ok();
    relay.wait().ok();
}

/// 24.10. prod deploy: after a Hive restart every presence was gone until
/// the relays said hello again ~5 min later, because a session that ended
/// with an error kept the backoff of an earlier outage. Here Hive is down
/// for two connects (the backoff grows), comes back, then restarts: the
/// relay is back with a hello within seconds, not after the old backoff.
#[test]
fn after_an_hive_restart_the_relay_says_hello_again_within_seconds() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let home = machine.home.path().to_path_buf();
    let mut relay = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(&home)
        .env("LEAN_CTX_HIVE_PREVIEW", "1")
        .env("HOME", &home)
        .env("LEAN_CTX_PROJECT_ROOT", &home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
        .env("LEAN_CTX_STATE_DIR", home.join("state"))
        .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .args(["hive", "bus", "relay", "--for", "90"])
        .spawn()
        .unwrap();
    wait_for("the relay's first hello", || fake.bus_hellos().len() == 1);

    // Down for two connects, then up again.
    fake.refuse_bus(2);
    fake.restart_bus();
    wait_for("a hello after the outage", || fake.bus_hellos().len() == 2);

    // A plain restart now: back within RECONNECT_FIRST + jitter (≤ 4 s).
    let restarted = std::time::Instant::now();
    fake.restart_bus();
    wait_for("a hello after the restart", || fake.bus_hellos().len() == 3);
    let back = restarted.elapsed();
    assert!(
        back < std::time::Duration::from_secs(8),
        "the relay came back after {back:?}"
    );
    assert_eq!(fake.bus_sessions(), 3, "one session per hello");
    relay.kill().ok();
    relay.wait().ok();
}

/// The bus CLI agents read with (lead, 25.10.): a mistyped subcommand fails
/// with exit 2 instead of printing the help with exit 0 (it looked like an
/// empty read), the help itself stays exit 0, and `agent read` names an
/// unknown bus id instead of reading nothing.
#[test]
fn agent_cli_refuses_unknown_subcommands_and_reads_once() {
    let machine = Machine::new();
    let home = machine.home.path();
    let typo = machine.run_in(home, &["agent", "reed", "--id", "x"]);
    assert_eq!(typo.status.code(), Some(2), "{typo:?}");
    assert!(
        String::from_utf8_lossy(&typo.stderr).contains("unknown subcommand 'reed'"),
        "{typo:?}"
    );
    let help = machine.run_in(home, &["agent"]);
    assert!(help.status.success(), "{help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("agent read --id"));
    let unknown = machine.run_in(home, &["agent", "read", "--id", "mcp-0-nobody"]);
    assert!(!unknown.status.success(), "{unknown:?}");
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("not registered"),
        "{unknown:?}"
    );
}

/// Engine metrics v2 against the fake wire: held back without the release
/// switch; with it, signed hourly buckets per vault, idempotent per hour and
/// rev; policy off sends nothing and drops the buffer (K3, K15, K16).
#[test]
fn engine_metrics_go_signed_per_vault_hour_and_stop_when_policy_turns_them_off() {
    use lean_ctx::core::hive::vault::{VaultMap, VaultRecord};
    use lean_ctx_protocol::hive::metrics_v2::{
        PlanDefault, PolicyLevel, SharingPolicyValueV2, SharingStateV2, SharingValue,
    };
    use sha2::Digest as _;
    const VAULT: &str = "3f9a1c7e5b2d4a6f8e0c1b3d5f7a9c2e";
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = machine.home.path().join("payments");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    // The ledger's spelling of the root (no Windows `\\?\` prefix), as its repo_hash is taken.
    let canonical = lean_ctx::core::pathutil::safe_canonicalize(&project).unwrap();
    let mut map = VaultMap::default();
    map.vaults.insert(
        "stable".into(),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: vec![canonical.to_string_lossy().into_owned()],
        },
    );
    map.save(machine.store().data_dir()).unwrap();
    // A verified effective policy is in force (no bundle = nothing is sent).
    let floor_dir = machine.store().data_dir().join("policy/effective");
    std::fs::create_dir_all(&floor_dir).unwrap();
    let effective = serde_json::json!({
        "schema": lean_ctx_protocol::hive::effective_policy::POLICY_DOCUMENT_SCHEMA,
        "security": {}, "context": {}, "sharing": {}
    });
    std::fs::write(
        floor_dir.join("floor.json"),
        serde_json::json!({
            "schema": lean_ctx::core::hive::effective_floor::FLOOR_SCHEMA,
            "version": 1, "digest": "sha256:test",
            "not_after": (chrono::Utc::now() + chrono::Duration::days(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "policy": {"effective": effective, "vaults": []}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(floor_dir.join("required.json"), r#"{"version":1}"#).unwrap();
    let digest = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
    let hash: String = hex::encode(digest)[..16].into();
    let ledger = machine.home.path().join("data/savings/ledger.jsonl");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let line = |saved: u64| {
        format!(
            "{}\n",
            serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "tool": "ctx_read", "baseline_tokens": saved * 2, "actual_tokens": saved,
                "saved_tokens": saved, "repo_hash": hash, "evidence_class": "measured",
            })
        )
    };
    std::fs::write(&ledger, line(500)).unwrap();
    let hours = || {
        fake.requests()
            .iter()
            .filter(|r| r.path == "/v1/metrics/hours")
            .count()
    };
    let sync_released = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
        let home = machine.home.path();
        command
            .current_dir(home)
            .env("LEAN_CTX_HIVE_PREVIEW", "1")
            .env("HOME", home)
            .env("LEAN_CTX_PROJECT_ROOT", home)
            .env("LEAN_CTX_DATA_DIR", home.join("data"))
            .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
            .env("LEAN_CTX_STATE_DIR", home.join("state"))
            .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .env("LEAN_CTX_HIVE_ENGINE_METRICS", "fake-wire")
            .args(["hive", "sync"]);
        command.output().unwrap()
    };

    // Not released: nothing is collected or sent, status says so.
    assert!(machine.run(&["sync"]).status.success());
    assert_eq!(hours(), 0, "held back until the release");
    let status = text(&machine.run(&["status"]).stdout);
    assert!(
        status.contains("Sharing engine metrics with") && status.contains("not released yet"),
        "{status}"
    );
    assert!(
        !machine
            .home
            .path()
            .join("data/hive/metrics-v2/buffer")
            .exists()
    );

    // Released for the fake wire: one signed bucket with the vault's slice.
    let sync = sync_released();
    assert!(sync.status.success(), "{sync:?}");
    assert_eq!(hours(), 1, "{sync:?}");
    let stored = fake.metrics_hours();
    assert_eq!(stored.len(), 1);
    let (_, rev, bucket) = &stored[0];
    let slice = &bucket["slices"][0];
    assert_eq!(slice["vault"], VAULT);
    assert_eq!(slice["tokens"]["saved"], 500);
    assert_eq!(slice["savings"]["measured"], 500);
    let body = &fake
        .requests()
        .into_iter()
        .find(|r| r.path == "/v1/metrics/hours")
        .unwrap()
        .body;
    for leaked in [
        canonical.to_string_lossy().as_ref(),
        hash.as_str(),
        "payments",
    ] {
        assert!(!body.contains(leaked), "{leaked} left the engine: {body}");
    }

    // Nothing new: nothing is resent; new activity: a higher rev replaces it.
    assert!(sync_released().status.success());
    assert_eq!(hours(), 1, "an acked hour is not sent again");
    std::fs::write(&ledger, line(500) + &line(300)).unwrap();
    assert!(sync_released().status.success());
    let stored = fake.metrics_hours();
    assert_eq!(stored.len(), 1, "one bucket per hour");
    assert!(stored[0].1 > *rev, "a higher rev replaced the hour");
    assert_eq!(stored[0].2["slices"][0]["tokens"]["saved"], 800);

    // The workspace turns the rule off: nothing is sent, nothing stays buffered.
    fake.set_metrics_sharing(Some(SharingStateV2 {
        state: SharingValue::Off,
        plan_default: PlanDefault::On,
        policy: Some(SharingPolicyValueV2 {
            value: false,
            level: PolicyLevel::Workspace,
        }),
        confirmed_at: None,
        since: None,
    }));
    std::fs::write(&ledger, line(500) + &line(300) + &line(100)).unwrap();
    let before = hours();
    let off = sync_released();
    assert!(off.status.success(), "{off:?}");
    assert!(
        text(&off.stdout).contains("Engine metrics not sent: off (policy, set by Team)"),
        "{off:?}"
    );
    assert_eq!(hours(), before, "policy off sends nothing");
    assert_eq!(
        fake.metrics_hours()[0].2["slices"][0]["tokens"]["saved"],
        800
    );
    let buffer = machine.home.path().join("data/hive/metrics-v2/buffer");
    assert!(
        !buffer.exists() || std::fs::read_dir(&buffer).unwrap().next().is_none(),
        "the buffer is dropped"
    );
}

/// `hive metrics confirm` is a person's act: never from a pipe or an agent.
#[test]
fn engine_metrics_confirmation_needs_a_terminal() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    // Before the release there is nothing to confirm, and nothing is asked.
    let unreleased = machine.run(&["metrics", "confirm"]);
    assert!(unreleased.status.success(), "{unreleased:?}");
    assert!(
        text(&unreleased.stdout).contains("not released yet"),
        "{unreleased:?}"
    );
    let home = machine.home.path();
    let confirm = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(home)
        .env("LEAN_CTX_HIVE_PREVIEW", "1")
        .env("HOME", home)
        .env("LEAN_CTX_PROJECT_ROOT", home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
        .env("LEAN_CTX_STATE_DIR", home.join("state"))
        .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .env("LEAN_CTX_HIVE_ENGINE_METRICS", "fake-wire")
        .args(["hive", "metrics", "confirm"])
        .output()
        .unwrap();
    assert!(!confirm.status.success(), "{confirm:?}");
    assert!(
        text(&confirm.stderr).contains("confirmed by a person in a terminal"),
        "{confirm:?}"
    );
    let preview = machine.run(&["metrics", "preview"]);
    assert!(preview.status.success(), "{preview:?}");
    let out = text(&preview.stdout);
    assert!(out.contains("Engine metrics."), "{out}");
    assert!(out.contains("/privacy/engine-metrics"), "{out}");
    assert!(
        fake.requests()
            .iter()
            .all(|r| !r.path.starts_with("/v1/metrics")),
        "preview and confirm send nothing"
    );
}

/// The preview for a sample hour, on an isolated engine against the fake
/// Hive (scratch HOME): prints what would be sent, sends nothing.
#[test]
fn engine_metrics_preview_shows_a_sample_hour_and_sends_nothing() {
    use lean_ctx::core::hive::vault::{VaultMap, VaultRecord};
    use sha2::Digest as _;
    const VAULT: &str = "3f9a1c7e5b2d4a6f8e0c1b3d5f7a9c2e";
    let fake = FakeHive::start();
    let machine = Machine::new();
    assert!(machine.connect(&fake).status.success());
    let project = machine.home.path().join("payments");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let canonical = lean_ctx::core::pathutil::safe_canonicalize(&project).unwrap();
    let mut map = VaultMap::default();
    map.vaults.insert(
        "stable".into(),
        VaultRecord {
            vault_id: VAULT.into(),
            ns: "acme".into(),
            name: "payments".into(),
            sealed: false,
            roots: vec![canonical.to_string_lossy().into_owned()],
        },
    );
    map.save(machine.store().data_dir()).unwrap();
    let digest = sha2::Sha256::digest(canonical.to_string_lossy().as_bytes());
    let hash: String = hex::encode(digest)[..16].into();
    // A sample hour: the last complete one.
    let hour = chrono::Utc::now() - chrono::Duration::hours(1);
    let at = |minute: i64| {
        let start = hour - chrono::Duration::minutes(i64::from(chrono::Timelike::minute(&hour)));
        (start + chrono::Duration::minutes(minute)).to_rfc3339()
    };
    let data = machine.home.path().join("data");
    std::fs::create_dir_all(data.join("savings")).unwrap();
    let ledger: String = [
        serde_json::json!({"ts": at(5), "tool": "ctx_read", "baseline_tokens": 12000, "actual_tokens": 2400, "saved_tokens": 9600, "repo_hash": hash, "evidence_class": "measured"}),
        serde_json::json!({"ts": at(20), "tool": "ctx_shell", "baseline_tokens": 3000, "actual_tokens": 900, "saved_tokens": 2100, "repo_hash": hash, "confidence": 0.8}),
        serde_json::json!({"ts": at(40), "tool": "ctx_search", "baseline_tokens": 5000, "actual_tokens": 1500, "saved_tokens": 3500, "repo_hash": hash, "evidence_class": "measured"}),
    ]
    .iter()
    .map(|l| l.to_string() + "\n")
    .collect();
    std::fs::write(data.join("savings/ledger.jsonl"), ledger).unwrap();
    let metering: String = [
        ("ctx_read", 12000, 2400, 9600, 5),
        ("ctx_read", 800, 800, 0, 9),
        ("ctx_shell", 3000, 900, 2100, 20),
        ("ctx_search", 5000, 1500, 3500, 40),
        ("ctx_tree", 300, 300, 0, 41),
    ]
    .iter()
    .map(|(tool, input, output, saved, minute)| {
        let ts = chrono::DateTime::parse_from_rfc3339(&at(*minute))
            .unwrap()
            .with_timezone(&chrono::Utc)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        serde_json::json!({"timestamp": ts, "tool_name": tool, "input_tokens": input,
            "output_tokens": output, "savings_tokens": saved, "project": hash})
        .to_string()
            + "\n"
    })
    .collect();
    std::fs::write(data.join("metering.jsonl"), metering).unwrap();
    let preview = |json: bool| {
        let home = machine.home.path();
        let mut args = vec!["hive", "metrics", "preview"];
        if json {
            args.push("--json");
        }
        Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .current_dir(home)
            .env("LEAN_CTX_HIVE_PREVIEW", "1")
            .env("HOME", home)
            .env("LEAN_CTX_PROJECT_ROOT", home)
            .env("LEAN_CTX_DATA_DIR", home.join("data"))
            .env("LEAN_CTX_CONFIG_DIR", home.join("config"))
            .env("LEAN_CTX_STATE_DIR", home.join("state"))
            .env("LEAN_CTX_CACHE_DIR", home.join("cache"))
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .args(&args)
            .output()
            .unwrap()
    };
    let human = preview(false);
    assert!(human.status.success(), "{human:?}");
    let out = text(&human.stdout);
    eprintln!("PREVIEW-BEGIN\n{out}PREVIEW-END");
    assert!(out.contains(VAULT), "{out}");
    assert!(out.contains("\"saved\": 15200"), "{out}");
    for leaked in [canonical.to_string_lossy().as_ref(), hash.as_str()] {
        assert!(!out.contains(leaked), "{leaked} in the preview: {out}");
    }
    // `--json` is the next batch: nothing while not released on this Hive
    // and nothing sent either way.
    let json = preview(true);
    assert!(json.status.success(), "{json:?}");
    let body: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(body["schema"], "leanctx.hive.engine-metrics/v2");
    assert!(
        fake.requests()
            .iter()
            .all(|r| !r.path.starts_with("/v1/metrics")),
        "the preview sends nothing"
    );
}

/// `config set hive.enabled false` stops local recording at once (the
/// `hive.` prefix check in the setter, K2 review 3 NEW-2), and `[hive]` is
/// the section the engine writes.
#[test]
fn config_set_hive_enabled_false_stops_recording_at_once() {
    let machine = Machine::new();
    let data = machine.home.path().join("data/hive");
    std::fs::create_dir_all(data.join("metrics-v2")).unwrap();
    std::fs::write(data.join("metrics-v2/secret-hits.on"), b"on\n").unwrap();
    std::fs::write(data.join("receipts.enabled"), b"1").unwrap();

    let out = machine.run_in(
        machine.home.path(),
        &["config", "set", "hive.enabled", "false", "--yes"],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(
        !data.join("metrics-v2/secret-hits.on").exists(),
        "secret-hit recording stops at once"
    );
    assert!(
        !data.join("receipts.enabled").exists(),
        "receipt recording stops at once"
    );
    // E2E 27.10. E1: a `[hive]` key set by `config set` is written there.
    let out = machine.run_in(
        machine.home.path(),
        &[
            "config",
            "set",
            "hive.url",
            "https://hive.example.com",
            "--yes",
        ],
    );
    assert!(out.status.success(), "{out:?}");
    let written = std::fs::read_to_string(machine.home.path().join("config/config.toml")).unwrap();
    assert!(written.contains("[hive]"), "{written}");
    assert!(written.contains("https://hive.example.com"), "{written}");
    assert!(!written.contains("atlas"), "{written}");
}
