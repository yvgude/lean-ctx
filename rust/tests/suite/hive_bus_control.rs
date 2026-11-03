// SPDX-License-Identifier: Apache-2.0

//! The bus relay against the fake Hive, and the controls it keeps
//! (`state_v2.json`) as tool admission and `hive status` see them, across
//! a lapsed lease and a disconnect, in a throwaway home.
#![cfg(feature = "http-server")]

use crate::support::fake_hive::{FakeHive, FakeOptions};
use lean_ctx_hive::HiveStore;
use lean_ctx_hive::bus::control;
use lean_ctx_hive::bus::v2::ControlAction;
use serde_json::json;
use std::path::Path;
use std::process::{Child, Command, Output};

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

    fn command(&self, args: &[&str]) -> Command {
        let home = self.home.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
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
            .arg("hive")
            .args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn relay(&self, seconds: &str) -> Child {
        self.command(&["bus", "relay", "--for", seconds])
            .spawn()
            .unwrap()
    }

    fn store(&self) -> HiveStore {
        HiveStore::at(home(self).join("config/hive"), home(self).join("data/hive"))
    }
}

/// `hive status --json`, parsed through the contract type and back.
fn status_json(machine: &Machine) -> serde_json::Value {
    let out = machine.run(&["status", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let typed: lean_ctx_protocol::hive::status::HiveCliStatusV1 =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {out:?}"));
    serde_json::to_value(typed).unwrap()
}

fn home(machine: &Machine) -> &Path {
    machine.home.path()
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

fn mode(store: &HiveStore, agent: Option<&str>) -> Option<ControlAction> {
    control::admission(store, agent).map(|refusal| refusal.action)
}

/// A 2.x Hive: the relay connects once the instance card answered.
fn hive_2x() -> FakeHive {
    FakeHive::start_with(FakeOptions {
        instance_card: true,
        ..FakeOptions::default()
    })
}

/// The C0 hold fixture as a bus control `action` with `id`, as the relay
/// keeps it after its signature verified.
fn bus_control(action: &str, id: &str, created_at: u64) -> serde_json::Value {
    let path = format!(
        "{}/../lean-ctx-protocol/tests/fixtures/hive-v2/bus/control.hold.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut control: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    control["target"] = json!("bus");
    control["action"] = json!(action);
    control["id"] = json!(id);
    control["created_at"] = json!(created_at);
    control.as_object_mut().unwrap().remove("worker_id");
    control
}

/// `hive.bus = "off"` reaches a running relay without a restart: it closes
/// its socket, and comes back once the bus is on again.
#[test]
fn switching_the_bus_off_stops_a_running_relay_without_a_restart() {
    let fake = hive_2x();
    let machine = Machine::new();
    let connect = machine.run(&["connect", "--url", &fake.url, "--no-browser"]);
    assert!(connect.status.success(), "{connect:?}");
    let config = home(&machine).join("config/config.toml");
    let on = std::fs::read_to_string(&config).unwrap();
    let mut relay = machine.relay("60");
    wait_for("the relay to connect", || !fake.bus_connected().is_empty());
    let off = if on.contains("[hive]") {
        on.replacen("[hive]", "[hive]\nbus = \"off\"", 1)
    } else {
        format!("{on}\n[hive]\nbus = \"off\"\n")
    };
    std::fs::write(&config, off).unwrap();
    wait_for("the relay to close its socket", || {
        fake.bus_connected().is_empty()
    });
    assert!(
        relay.try_wait().unwrap().is_none(),
        "paused, not ended: it resumes when the bus is on again"
    );
    std::fs::write(&config, on).unwrap();
    wait_for("the relay to reconnect", || {
        !fake.bus_connected().is_empty()
    });
    relay.kill().ok();
    relay.wait().ok();
}

/// Without an answered instance card the relay never opens a socket: it
/// asks for the card again and waits (a Hive that does not speak 2.x).
#[test]
fn without_an_instance_card_the_relay_waits_and_opens_no_socket() {
    let fake = FakeHive::start();
    let machine = Machine::new();
    let connect = machine.run(&["connect", "--url", &fake.url, "--no-browser"]);
    assert!(connect.status.success(), "{connect:?}");
    let relay = machine.run(&["bus", "relay", "--for", "3"]);
    assert!(relay.status.success(), "{relay:?}");
    assert_eq!(fake.bus_sessions(), 0, "no bus socket");
    assert!(
        fake.requests()
            .iter()
            .any(|r| r.method == "PATCH" && r.path.ends_with("/card")),
        "the relay asked for the instance card"
    );
}

/// The controls the relay keeps gate Hive traffic and reach `hive status`;
/// a lapsed lease voids them and they come back with a valid one (plan C
/// rule 2); disconnected, they no longer gate.
#[test]
fn kept_controls_gate_and_show_until_the_lease_lapses_or_the_machine_disconnects() {
    let fake = hive_2x();
    let machine = Machine::new();
    let connect = machine.run(&[
        "connect",
        "--url",
        &fake.url,
        "--no-browser",
        "--label",
        "control-box",
    ]);
    assert!(connect.status.success(), "{connect:?}");
    let store = machine.store();
    assert!(mode(&store, Some("claude-a")).is_none());

    let state = home(&machine).join("data/hive/bus/state_v2.json");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    let kept = json!({"controls": {
        "ctl_1": bus_control("hold", "ctl_1", 1_792_800_000_000),
        "ctl_2": bus_control("stop", "ctl_2", 1_792_800_001_000),
    }});
    std::fs::write(&state, serde_json::to_vec(&kept).unwrap()).unwrap();
    assert_eq!(
        mode(&store, Some("claude-a")),
        Some(ControlAction::Stop),
        "a stop wins over a hold"
    );
    assert_eq!(
        mode(&store, None),
        Some(ControlAction::Stop),
        "unknown callers too"
    );
    let status = status_json(&machine);
    assert_eq!(
        status["bus_controls"].as_array().unwrap().len(),
        2,
        "{status}"
    );
    assert!(status.get("bus_controls_inactive").is_none(), "{status}");

    // A lapsed lease (service clock 30 days ahead) voids them, and they come
    // back with a valid lease.
    let lease_path = home(&machine).join("config/hive/lease.json");
    let original = std::fs::read(&lease_path).unwrap();
    let mut lapsed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    lapsed["offset_seconds"] = json!(lapsed["offset_seconds"].as_i64().unwrap() + 30 * 86_400);
    std::fs::write(&lease_path, serde_json::to_vec(&lapsed).unwrap()).unwrap();
    assert!(mode(&store, Some("claude-a")).is_none());
    assert_eq!(
        control::status_at(&store, chrono::Utc::now()).1,
        Some(control::Inactive::LeaseLapsed)
    );
    let status = status_json(&machine);
    assert_eq!(status["bus_controls_inactive"], "lease_lapsed", "{status}");
    assert!(
        status.get("bus_controls").is_none(),
        "absent when empty: {status}"
    );
    std::fs::write(&lease_path, &original).unwrap();
    assert_eq!(mode(&store, Some("claude-a")), Some(ControlAction::Stop));

    // Disconnected (local data kept): the kept controls no longer gate.
    let output = machine.run(&["disconnect", "--keep-local"]);
    assert!(output.status.success(), "{output:?}");
    assert!(control::admission(&store, Some("claude-a")).is_none());
    assert!(control::status_at(&store, chrono::Utc::now()).0.is_empty());
}
