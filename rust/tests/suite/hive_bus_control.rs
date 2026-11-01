// SPDX-License-Identifier: Apache-2.0

//! Bus control frames (`hold`, `veto`, `stop`, `resume`) from the fake Hive
//! through the relay into the admission state, across a reconnect and a
//! disconnect, in a throwaway home.

use crate::support::fake_hive::FakeHive;
use lean_ctx::core::hive::HiveStore;
use lean_ctx::core::hive::bus::control;
use lean_ctx_protocol::hive::bus::ControlAction;
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
    control::admission(store, agent).map(|refusal| refusal.entry.action)
}

/// `hive.bus = "off"` reaches a running relay without a restart: it closes
/// its socket, and comes back once the bus is on again.
#[test]
fn switching_the_bus_off_stops_a_running_relay_without_a_restart() {
    let fake = FakeHive::start();
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

#[test]
fn control_frames_gate_agents_survive_replay_and_end_with_the_enrollment() {
    let fake = FakeHive::start();
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

    let mut relay = machine.relay("30");
    wait_for("the relay to connect", || !fake.bus_connected().is_empty());
    let control = |seq: u64, target: Option<&str>, action: &str, target_ref: Option<&str>| {
        let mut frame = json!({
            "control_id": format!("ctl_{seq}"), "action": action,
            "reason": "Owner review of the contract.", "issued_by": "admin@acme", "seq": seq,
        });
        if let Some(target) = target {
            frame["target_agent"] = json!(target);
        }
        if let Some(target_ref) = target_ref {
            frame["target_ref"] = json!(target_ref);
        }
        fake.send_control(frame);
    };

    control(1, None, "hold", None);
    wait_for("hold on every agent", || {
        mode(&store, Some("claude-b")) == Some(ControlAction::Hold)
    });
    assert_eq!(
        mode(&store, None),
        Some(ControlAction::Hold),
        "unknown callers too"
    );

    control(2, Some("claude-a"), "veto", Some("lease_7"));
    control(3, Some("claude-a"), "stop", None);
    wait_for("stop on claude-a", || {
        mode(&store, Some("claude-a")) == Some(ControlAction::Stop)
    });
    assert!(control::vetoed(&store, Some("claude-a"), "lease_7").is_some());
    assert!(control::vetoed(&store, Some("claude-b"), "lease_7").is_none());

    control(4, None, "resume", None);
    wait_for("resume of every agent", || {
        mode(&store, Some("claude-b")).is_none()
    });
    assert_eq!(
        mode(&store, Some("claude-a")),
        Some(ControlAction::Stop),
        "an agent's own stop outlives a resume of *"
    );
    relay.kill().ok();
    relay.wait().ok();

    // Reconnect: the fake replays all four frames; none is newer, nothing changes.
    let replay = machine.run(&["bus", "relay", "--for", "3"]);
    assert!(replay.status.success(), "{replay:?}");
    assert!(
        mode(&store, Some("claude-b")).is_none(),
        "the old hold stays resumed"
    );
    assert_eq!(mode(&store, Some("claude-a")), Some(ControlAction::Stop));
    let active = control::active(&store);
    assert_eq!(active.len(), 2, "{active:?}");
    let status = status_json(&machine);
    assert_eq!(
        status["bus_controls"].as_array().unwrap().len(),
        2,
        "{status}"
    );
    assert!(status.get("bus_controls_inactive").is_none(), "{status}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let file = home(&machine).join("data/hive/bus/control.json");
        let mode = std::fs::metadata(file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    // A lapsed lease (service clock 30 days ahead) voids them, and they come
    // back with a valid lease (plan C rule 2).
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

    // Disconnected (local data kept): controls of that enrollment no longer gate.
    let output = machine.run(&["disconnect", "--keep-local"]);
    assert!(output.status.success(), "{output:?}");
    assert!(home(&machine).join("data/hive/bus/control.json").exists());
    assert!(control::admission(&store, Some("claude-a")).is_none());
    assert!(control::active(&store).is_empty());
}
