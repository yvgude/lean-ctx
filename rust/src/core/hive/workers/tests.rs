// SPDX-License-Identifier: Apache-2.0
//! Worker identity against C0 2.0 `vectors/worker_id.vectors.json` and
//! `vectors/signing.vectors.json` (test-only keys).

use super::attach::{WORKER_CAP, attach_in, detect_host_app, host_display};

/// Dogfood finding 25.10.: a 1.x enrolment stores `inst_<32 hex>`, the
/// server hashes the dashed UUID; both spellings give the server's id.
#[test]
fn the_worker_id_hashes_the_canonical_instance_id() {
    let uuid = "0a80ace1-52f1-49c2-85f7-6991bada0099";
    for spelling in [
        "inst_0a80ace152f149c285f76991bada0099",
        "0A80ACE1-52F1-49C2-85F7-6991BADA0099",
        "0a80ace152f149c285f76991bada0099",
        uuid,
    ] {
        assert_eq!(canonical_instance_id(spelling), uuid, "{spelling}");
        assert_eq!(
            workspace_worker_id(spelling, "codex", "backend"),
            workspace_worker_id(uuid, "codex", "backend")
        );
    }
    let expected = {
        let digest = blake3::hash(format!("{uuid}|codex|backend").as_bytes());
        format!("wkr_{}", &base32_lower(digest.as_bytes())[..20])
    };
    assert_eq!(
        workspace_worker_id("inst_0a80ace152f149c285f76991bada0099", "codex", "backend"),
        expected
    );
    // Anything that is not a UUID in some spelling stays as it is.
    assert_eq!(canonical_instance_id("inst_short"), "inst_short");
}

#[test]
fn a_book_from_the_inst_spelling_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let legacy = "inst_0a80ace152f149c285f76991bada0099";
    std::fs::create_dir_all(store.data_dir()).unwrap();
    let old_id = {
        let digest = blake3::hash(format!("{legacy}|codex|backend").as_bytes());
        format!("wkr_{}", &base32_lower(digest.as_bytes())[..20])
    };
    std::fs::write(
        store.data_dir().join("workers.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "instance_id": legacy,
            "workers": [{"worker_id": old_id, "host_app": "codex", "role": "backend"}],
        }))
        .unwrap(),
    )
    .unwrap();
    let attached = super::attach::attach(&store, legacy, "codex", "backend").unwrap();
    assert_eq!(
        attached.worker.worker_id,
        workspace_worker_id(legacy, "codex", "backend")
    );
    assert_ne!(attached.worker.worker_id, old_id);
}
use super::card::{Availability, Budget, ModelSource, WorkerCard, self_reported_model};
use super::*;
use crate::core::hive::HiveStore;

const SEED_A: [u8; 32] = [0x49; 32];
const SEED_B: [u8; 32] = [0x4a; 32];
const INSTANCE_A: &str = "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b";
const INSTANCE_B: &str = "8e3c0d5f-2b4e-4f60-9b8c-1d2e3f4a5b6c";
const SWARM: &str = "swarm:9a4d7e21-6b3c-4f58-8e09-1d2c3b4a5f02";
const VAULT: &str = "vault:3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";

fn keys(seed: [u8; 32]) -> InstanceKeys {
    InstanceKeys::from_seeds(seed, [0x01; 32])
}

#[test]
fn roles_normalise_like_the_contract() {
    for (input, output) in [
        ("backend", "backend"),
        ("Backend Dev", "backend-dev"),
        ("  QA / Review!!  ", "qa-review"),
        ("", "general"),
        ("---", "general"),
        ("Ünïcode Rôle", "n-code-r-le"),
        (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-yz",
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        ),
    ] {
        assert_eq!(normalize_role(input), output, "{input:?}");
    }
}

#[test]
fn workspace_ids_match_the_vectors() {
    for (instance, host, role, id) in [
        (
            INSTANCE_A,
            "claude-code",
            "backend",
            "wkr_vk4oelpv5lzjmev44hun",
        ),
        (INSTANCE_A, "codex", "backend", "wkr_kevgmpb7tmes3ii4aly6"),
        (
            INSTANCE_B,
            "claude-code",
            "backend",
            "wkr_jnt2cjplb6j4ugrq4pxw",
        ),
        (INSTANCE_B, "codex", "reviewer", "wkr_gawyravj36igdp3irdfo"),
    ] {
        assert_eq!(workspace_worker_id(instance, host, role), id);
    }
}

#[test]
fn alias_key_ids_and_hlc_nodes_match_the_vectors() {
    let vectors = signing_vectors();
    for (seed, name) in [(SEED_A, "instance_a"), (SEED_B, "instance_b")] {
        assert_eq!(
            hex::encode(WorkerAliasKey::from_seed(&seed).0.as_ref()),
            vectors["keys"][name]["worker_alias_key_hex"]
                .as_str()
                .unwrap(),
            "{name}"
        );
    }
    let seed_of = |instance: &str| {
        if instance == "instance_a" {
            SEED_A
        } else {
            SEED_B
        }
    };
    for worker in vectors["workers"].as_array().unwrap() {
        let Some(place) = worker["place"].as_str() else {
            continue;
        };
        let seed = seed_of(worker["instance"].as_str().unwrap());
        assert_eq!(
            WorkerAliasKey::from_seed(&seed).worker_id(
                worker["host_app"].as_str().unwrap(),
                worker["role"].as_str().unwrap(),
                place
            ),
            worker["worker_id"].as_str().unwrap()
        );
    }
    for alias in vectors["aliases"].as_array().unwrap() {
        let seed = seed_of(alias["instance"].as_str().unwrap());
        let place = alias["place"].as_str().unwrap();
        assert_eq!(
            alias_hlc_node(&keys(seed), place),
            alias["hlc_node"].as_str().unwrap()
        );
    }
}

#[test]
fn alias_signing_keys_match_the_vectors() {
    let vectors = signing_vectors();
    for alias in vectors["aliases"].as_array().unwrap() {
        let seed = if alias["instance"] == "instance_a" {
            SEED_A
        } else {
            SEED_B
        };
        let key = alias_signing_key(&keys(seed), alias["place"].as_str().unwrap());
        assert_eq!(
            hex::encode(key.to_bytes()),
            alias["seed_hex"].as_str().unwrap()
        );
        assert_eq!(key_id(&key), alias["key_id"].as_str().unwrap());
    }
    assert_eq!(
        key_id(keys(SEED_A).signing_key()),
        vectors["keys"]["instance_a"]["key_id"].as_str().unwrap()
    );
}

/// The contract's signing vectors (byte-identical mirror of the Hive repo).
fn signing_vectors() -> serde_json::Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/vectors/signing.vectors.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path)
}

#[test]
fn one_engine_is_unlinkable_across_places_and_to_its_instance_id() {
    let a = WorkerAliasKey::from_seed(&SEED_A);
    let (in_swarm, in_vault) = (
        a.worker_id("codex", "general", SWARM),
        a.worker_id("codex", "general", VAULT),
    );
    assert_ne!(in_swarm, in_vault);
    assert_ne!(a.hlc_node(SWARM), a.hlc_node(VAULT));
    // A co-member knows the instance UUID, not the seed: nothing derived
    // from the UUID reproduces the alias or the node.
    let guess = workspace_worker_id(INSTANCE_A, "codex", "general");
    assert_ne!(guess, in_swarm);
    let unkeyed = blake3::hash(format!("codex|general|{SWARM}").as_bytes());
    assert_ne!(worker_id_of(unkeyed.as_bytes()), in_swarm);
    assert_ne!(
        &blake3::hash(SWARM.as_bytes()).to_hex()[..16],
        a.hlc_node(SWARM)
    );
    let keyed_by_uuid = blake3::keyed_hash(
        blake3::hash(INSTANCE_A.as_bytes()).as_bytes(),
        SWARM.as_bytes(),
    );
    assert_ne!(&keyed_by_uuid.to_hex()[..16], a.hlc_node(SWARM));
}

#[test]
fn names_follow_the_contract_rule_in_its_order() {
    use NameRefusal::*;
    for (input, result) in [
        ("yves-mac · Claude Code · backend", None),
        ("Codex · general", None),
        ("Пётр · reviewer", Some(Confusable)),
        ("Pаypal bot", Some(Confusable)),
        ("аdmin", Some(Confusable)),
        ("my hive helper", Some(Reserved)),
        ("LeanCTX bot", Some(Reserved)),
        ("", Some(Length)),
        (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Some(Length),
        ),
        ("emoji 😀 bot", Some(Charset)),
        ("ｆｕｌｌｗｉｄｔｈ ok", None),
        ("日本 bot", Some(Confusable)),
    ] {
        assert_eq!(check_name(input).err(), result, "{input:?}");
    }
    assert_eq!(check_name("ｆｕｌｌｗｉｄｔｈ ok").unwrap(), "fullwidth ok");
}

#[test]
fn default_names_pass_the_rule() {
    assert_eq!(
        default_name("yves-mac", "Claude Code", "backend"),
        "yves-mac · Claude Code · backend"
    );
    assert_eq!(default_name("", "Codex", "general"), "Codex · general");
    // A label that cannot pass (mixed alphabets, reserved word) is left out.
    assert_eq!(
        default_name("Пётр-mac", "Codex", "general"),
        "Codex · general"
    );
    assert_eq!(
        default_name("hive-box", "Codex", "general"),
        "Codex · general"
    );
    assert_eq!(
        default_name("my 😀 mac", "Codex", "general"),
        "my mac · Codex · general"
    );
}

#[test]
fn six_host_apps_are_detected() {
    let none: [(&str, &str); 0] = [];
    let env = |pairs: &[(&'static str, &'static str)]| pairs.to_vec();
    assert_eq!(
        detect_host_app(None, None, env(&[("CLAUDECODE", "1")]), None),
        "claude-code"
    );
    assert_eq!(
        detect_host_app(None, None, env(&[("CODEX_THREAD_ID", "t")]), None),
        "codex"
    );
    assert_eq!(
        detect_host_app(None, None, env(&[("CURSOR_TRACE_ID", "x")]), None),
        "cursor"
    );
    assert_eq!(
        detect_host_app(None, None, env(&[("GEMINI_CLI", "1")]), None),
        "gemini-cli"
    );
    assert_eq!(
        detect_host_app(None, None, env(&[("COPILOT_AGENT", "1")]), None),
        "copilot"
    );
    assert_eq!(
        detect_host_app(None, None, env(&[("OPENCODE", "1")]), None),
        "opencode"
    );
    assert_eq!(
        detect_host_app(None, None, none, Some("cursor-agent")),
        "cursor"
    );
    // The handshake and the agent type outrank the environment.
    assert_eq!(
        detect_host_app(Some("codex"), None, env(&[("CLAUDECODE", "1")]), None),
        "codex"
    );
    assert_eq!(
        detect_host_app(Some("unknown"), Some("claude"), none, None),
        "claude-code"
    );
    assert_eq!(
        detect_host_app(None, None, none, Some("Zed Editor/1.0 ✓")),
        "mcp-client:Zed Editor1.0"
    );
    assert_eq!(
        detect_host_app(None, None, none, None),
        "mcp-client:unknown"
    );
    assert_eq!(host_display("claude-code"), "Claude Code");
    assert_eq!(host_display("mcp-client:Zed"), "Zed");
}

#[test]
fn the_thirty_third_role_maps_to_general() {
    let mut workers = Vec::new();
    let first = attach_in(&mut workers, INSTANCE_A, "codex", "General");
    assert_eq!(first.worker.role, "general");
    for n in 1..WORKER_CAP {
        let a = attach_in(&mut workers, INSTANCE_A, "codex", &format!("role-{n}"));
        assert!(!a.cap_reached);
    }
    assert_eq!(workers.len(), WORKER_CAP);
    let again = attach_in(&mut workers, INSTANCE_A, "codex", "role-7");
    assert_eq!(again.worker.role, "role-7");
    assert!(!again.cap_reached);
    let over = attach_in(&mut workers, INSTANCE_A, "codex", "one-too-many");
    assert!(over.cap_reached);
    assert_eq!(over.worker, first.worker);
    assert_eq!(workers.len(), WORKER_CAP);
}

#[test]
fn the_book_persists_and_resets_for_another_instance() {
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let a = attach::attach(&store, INSTANCE_A, "claude-code", "backend").unwrap();
    assert_eq!(a.worker.worker_id, "wkr_vk4oelpv5lzjmev44hun");
    attach::attach(&store, INSTANCE_A, "claude-code", "Backend").unwrap();
    assert_eq!(attach::list(&store, INSTANCE_A).unwrap().len(), 1);
    assert!(attach::list(&store, INSTANCE_B).unwrap().is_empty());
}

/// A session as the registry keeps it (one per process; distinct pids).
fn session(
    agent_id: &str,
    agent_type: &str,
    role: Option<&str>,
    pid: u32,
) -> crate::core::agents::AgentEntry {
    let now = chrono::Utc::now();
    crate::core::agents::AgentEntry {
        agent_id: agent_id.into(),
        agent_type: agent_type.into(),
        role: role.map(str::to_string),
        project_root: "/tmp/p".into(),
        started_at: now,
        last_active: now,
        pid,
        process_identity: None,
        durable_identity_id: None,
        status: crate::core::agents::AgentStatus::Active,
        status_message: None,
        transport: None,
        role_source: None,
        parent: None,
    }
}

/// V5.3: `ctx_agent list` shows workers; process ids are not agents.
#[test]
fn the_roster_lists_workers_not_processes() {
    let sessions = [
        session("mcp-4101-a", "claude", Some("Backend"), 4101),
        session("mcp-4102-b", "claude", Some("backend"), 4102),
        session("mcp-4103-c", "codex", None, 4103),
    ];
    let agents: Vec<&crate::core::agents::AgentEntry> = sessions.iter().collect();
    let now = chrono::Utc::now();
    let enrolled = roster::render(&agents, Some((INSTANCE_A, &[])), now);
    let expected = format!(
        "Active workers (2):\n  wkr_vk4oelpv5lzjmev44hun Claude Code · backend state=active (2 sessions, last active 0m ago)\n      session mcp-4101-a status=active\n      session mcp-4102-b status=active\n  {} Codex · general state=active (1 session, last active 0m ago)\n      session mcp-4103-c status=active\n",
        workspace_worker_id(INSTANCE_A, "codex", "general"),
    );
    assert_eq!(enrolled, expected);
    assert!(!enrolled.contains("pid"));
    let local = roster::render(&agents, None, now);
    assert!(local.starts_with("Active workers (2):\n  (not on Hive) Claude Code · backend"));
}

#[test]
fn cards_stay_within_the_contract_bounds() {
    let card = WorkerCard::new(
        "claude-code",
        Some(" claude-opus-5-5 "),
        ["ctx_shell", "ctx_read", "ctx_read", ""].map(String::from),
        Budget {
            tasks_per_day: Some(500),
            usd_cents_per_day: Some(90_000),
        },
        Availability::Online,
    );
    assert_eq!(card.model, "claude-opus-5-5");
    assert_eq!(card.model_source, ModelSource::SelfReported);
    assert_eq!(card.tools, ["ctx_read", "ctx_shell"]);
    assert_eq!(card.budget.tasks_per_day, Some(200));
    assert_eq!(card.budget.usd_cents_per_day, Some(50_000));
    let bare = WorkerCard::new(
        "codex",
        None,
        Vec::new(),
        Budget::default(),
        Availability::Idle,
    );
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        serde_json::json!({"host_app": "codex", "model": "unknown", "model_source": "unknown",
            "tools": [], "budget": {}, "availability": "idle"})
    );
    assert_eq!(
        self_reported_model([("CODEX_MODEL", "gpt"), ("ANTHROPIC_MODEL", "opus")]),
        Some("opus".to_string())
    );
}
