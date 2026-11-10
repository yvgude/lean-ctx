// SPDX-License-Identifier: Apache-2.0
//! Worker identity against C0 2.0 `vectors/worker_id.vectors.json` and
//! `vectors/signing.vectors.json` (test-only keys).

use super::attach::{WORKER_CAP, attach_in, detect_host_app, host_display};

/// Task #21: a role is made per app session, stays a valid normalised role
/// (so Hive recomputes the same `wkr_` id from it) and is stable.
#[test]
fn a_session_role_is_per_session_stable_and_a_valid_role() {
    use super::{normalize_role, session_role};
    assert_eq!(session_role("Coder", None), "coder");
    assert_eq!(
        session_role("coder", Some("  ")),
        "coder",
        "blank: no session"
    );
    let a = session_role("coder", Some("sess-a"));
    assert_eq!(a, session_role("coder", Some("sess-a")), "stable");
    assert_ne!(a, session_role("coder", Some("sess-b")), "per session");
    assert_ne!(a, session_role("reviewer", Some("sess-a")), "per role");
    assert!(
        a.starts_with("coder-s") && a.len() == "coder-s".len() + 8,
        "{a}"
    );
    let long = session_role("a-very-long-role-name-that-goes-past-32", Some("sess-a"));
    assert!(long.len() <= 32, "{long}");
    let cut_on_dash = session_role("abcdefghijklmnopqrstu-wxyz", Some("sess-a"));
    assert!(!cut_on_dash.contains("--"), "{cut_on_dash}");
    for role in [a, long, cut_on_dash, session_role("", Some("x"))] {
        assert_eq!(normalize_role(&role), role, "already normalised");
    }
    let instance = "0a80ace1-52f1-49c2-85f7-6991bada0099";
    assert_ne!(
        workspace_worker_id(instance, "claude-code", &session_role("coder", Some("w"))),
        workspace_worker_id(instance, "claude-code", &session_role("coder", Some("r"))),
    );
}

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
use crate::HiveStore;

const SEED_A: [u8; 32] = [0x49; 32];
const SEED_B: [u8; 32] = [0x4a; 32];
const INSTANCE_A: &str = "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b";
const INSTANCE_B: &str = "8e3c0d5f-2b4e-4f60-9b8c-1d2e3f4a5b6c";
const OTHER_VAULT: &str = "vault:c5e8a1f3-2d7b-4e96-a4c0-7b1d9e3f6a03";
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
        concat!(env!("CARGO_MANIFEST_DIR"), "/../..")
    );
    serde_json::from_slice(&std::fs::read(&path).expect(&path)).expect(&path)
}

#[test]
fn one_engine_is_unlinkable_across_places_and_to_its_instance_id() {
    let a = WorkerAliasKey::from_seed(&SEED_A);
    let (in_other, in_vault) = (
        a.worker_id("codex", "general", OTHER_VAULT),
        a.worker_id("codex", "general", VAULT),
    );
    assert_ne!(in_other, in_vault);
    assert_ne!(a.hlc_node(OTHER_VAULT), a.hlc_node(VAULT));
    // A co-member knows the instance UUID, not the seed: nothing derived
    // from the UUID reproduces the alias or the node.
    let guess = workspace_worker_id(INSTANCE_A, "codex", "general");
    assert_ne!(guess, in_other);
    let unkeyed = blake3::hash(format!("codex|general|{OTHER_VAULT}").as_bytes());
    assert_ne!(worker_id_of(unkeyed.as_bytes()), in_other);
    assert_ne!(
        &blake3::hash(OTHER_VAULT.as_bytes()).to_hex()[..16],
        a.hlc_node(OTHER_VAULT)
    );
    let keyed_by_uuid = blake3::keyed_hash(
        blake3::hash(INSTANCE_A.as_bytes()).as_bytes(),
        OTHER_VAULT.as_bytes(),
    );
    assert_ne!(&keyed_by_uuid.to_hex()[..16], a.hlc_node(OTHER_VAULT));
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

/// Dogfood 03.11.: the owner's machine had 32 workers from roles of every
/// project, so a new `ops` session shared the `capture` worker and Hive
/// refused its claim (presence with another role). A full book now gives
/// up the longest-unused worker without a session; a registered one is
/// queued for retirement on Hive, and coming back un-queues it.
#[test]
fn a_full_book_gives_the_longest_unused_slot_to_a_new_role() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    for n in 0..WORKER_CAP {
        attach::attach(&store, INSTANCE_A, "claude-code", &format!("role-{n}")).unwrap();
    }
    // Registered, in order: role-0 is the oldest.
    attach::update_book(&store, INSTANCE_A, |workers| {
        for (n, w) in workers.iter_mut().enumerate() {
            w.registered_at = Some(1_000 + n as u64);
            w.last_attached = Some(1_000 + n as u64);
        }
    })
    .unwrap();
    let gone = workspace_worker_id(INSTANCE_A, "claude-code", "role-0");

    let ops = attach::attach(&store, INSTANCE_A, "claude-code", "ops").unwrap();
    assert_eq!(ops.worker.role, "ops");
    assert!(!ops.cap_reached, "the new role has its own worker");
    let book = attach::list(&store, INSTANCE_A).unwrap();
    assert_eq!(book.len(), WORKER_CAP);
    assert!(book.iter().all(|w| w.worker_id != gone), "role-0 gave way");
    assert_eq!(
        attach::retired(&store, INSTANCE_A).unwrap(),
        vec![gone.clone()]
    );

    // role-0 returns before Hive retired it: it is no longer to be retired.
    let back = attach::attach(&store, INSTANCE_A, "claude-code", "role-0").unwrap();
    assert_eq!(back.worker.worker_id, gone, "the same deterministic id");
    let retired = attach::retired(&store, INSTANCE_A).unwrap();
    assert!(!retired.contains(&gone), "{retired:?}");
    assert_eq!(retired.len(), 1, "role-1 gave way for it");

    attach::retirement_done(&store, INSTANCE_A, &retired[0]).unwrap();
    assert!(attach::retired(&store, INSTANCE_A).unwrap().is_empty());
}

/// Workers a live session holds, and the connector's own (capture,
/// friction, CLI), never give way; when nothing can, the session shares
/// as before and `cap_reached` says so.
#[test]
fn workers_in_use_and_the_connectors_own_stay() {
    let mut workers = Vec::new();
    attach_in(&mut workers, INSTANCE_A, "lean-ctx", "capture");
    for n in 1..WORKER_CAP {
        attach_in(&mut workers, INSTANCE_A, "codex", &format!("role-{n}"));
    }
    for (n, w) in workers.iter_mut().enumerate() {
        w.registered_at = Some(n as u64);
        w.last_attached = Some(n as u64);
    }
    let mut retired = Vec::new();
    let mut in_use: std::collections::BTreeSet<(String, String)> = (1..WORKER_CAP)
        .filter(|n| *n != 5)
        .map(|n| ("codex".to_string(), format!("role-{n}")))
        .collect();
    assert!(attach::evict_one(&mut workers, &mut retired, &in_use));
    assert_eq!(
        retired,
        vec![workspace_worker_id(INSTANCE_A, "codex", "role-5")],
        "the oldest worker is capture, but only role-5 is free"
    );
    in_use.insert(("codex".to_string(), "role-5".to_string()));
    let before = workers.len();
    assert!(!attach::evict_one(&mut workers, &mut retired, &in_use));
    assert_eq!(workers.len(), before, "nothing free: nothing leaves");
    assert!(workers.iter().any(|w| w.role == "capture"));
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
) -> lean_ctx::core::agents::AgentEntry {
    let now = chrono::Utc::now();
    lean_ctx::core::agents::AgentEntry {
        agent_id: agent_id.into(),
        agent_type: agent_type.into(),
        role: role.map(str::to_string),
        project_root: "/tmp/p".into(),
        started_at: now,
        last_active: now,
        pid,
        process_identity: None,
        durable_identity_id: None,
        status: lean_ctx::core::agents::AgentStatus::Active,
        status_message: None,
        transport: None,
        role_source: None,
        parent: None,
        app_session_id: None,
        display_name: None,
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
    let agents: Vec<&lean_ctx::core::agents::AgentEntry> = sessions.iter().collect();
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

/// Review 5!246 #1: `hive worker retire <wkr>` refuses a worker an active
/// session holds (Hive would delete a worker that keeps posting), also when
/// the book does not have it; `--force` retires it anyway.
#[test]
fn retiring_a_worker_in_use_is_refused_unless_forced() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let project = tempfile::tempdir().unwrap();
    let root = project.path().to_string_lossy().into_owned();
    let mut registry = lean_ctx::core::agents::AgentRegistry::new();
    let agent = registry
        .register("claude", Some("queen"), &root, None)
        .unwrap();
    registry.save().unwrap();
    let entry = registry
        .agents
        .iter()
        .find(|a| a.agent_id == agent)
        .unwrap();
    let (host, role) = attach::agent_worker_key(entry);
    let held = attach::attach(&store, INSTANCE_A, &host, &role)
        .unwrap()
        .worker;
    attach::update_book(&store, INSTANCE_A, |workers| {
        for w in workers.iter_mut() {
            w.registered_at = Some(1);
        }
    })
    .unwrap();
    let ids = vec![held.worker_id.clone()];

    let refused = attach::give_up(&store, INSTANCE_A, &ids, false).unwrap_err();
    assert!(refused.to_string().contains("in use"), "{refused}");
    assert_eq!(attach::list(&store, INSTANCE_A).unwrap().len(), 1);
    assert!(attach::retired(&store, INSTANCE_A).unwrap().is_empty());

    // Not in the book: the id is still the live session's worker.
    attach::update_book(&store, INSTANCE_A, Vec::clear).unwrap();
    assert!(attach::give_up(&store, INSTANCE_A, &ids, false).is_err());
    assert!(attach::retired(&store, INSTANCE_A).unwrap().is_empty());

    assert_eq!(
        attach::give_up(&store, INSTANCE_A, &ids, true).unwrap(),
        ids
    );
    assert_eq!(attach::retired(&store, INSTANCE_A).unwrap(), ids);
}

/// Review 5!246 #2: an agent registry that cannot be parsed is not "no
/// session": the idle sweeps and an explicit retirement give up nothing.
#[test]
fn sweeps_retire_nothing_when_the_agent_registry_is_unreadable() {
    let _data = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
    let id = attach::attach(&store, INSTANCE_A, "codex", "reviewer")
        .unwrap()
        .worker
        .worker_id;
    attach::update_book(&store, INSTANCE_A, |workers| {
        for w in workers.iter_mut() {
            w.registered_at = Some(1);
            w.last_attached = Some(1);
            w.per_session = true;
        }
    })
    .unwrap();
    let agents = lean_ctx::core::data_dir::lean_ctx_data_dir()
        .unwrap()
        .join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("registry.json"), b"{ not json").unwrap();

    let idle = std::time::Duration::ZERO;
    assert!(attach::give_up_idle(&store, INSTANCE_A, idle, false).is_err());
    assert!(attach::give_up_idle(&store, INSTANCE_A, idle, true).is_err());
    assert!(attach::give_up(&store, INSTANCE_A, &[id], false).is_err());
    assert_eq!(attach::list(&store, INSTANCE_A).unwrap().len(), 1);
    assert!(attach::retired(&store, INSTANCE_A).unwrap().is_empty());
}
