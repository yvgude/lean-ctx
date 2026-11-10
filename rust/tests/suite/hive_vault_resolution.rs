// SPDX-License-Identifier: Apache-2.0

//! Dogfood 5.11.: one repository split across two vaults without a
//! warning. Each test replays the sequence against the fake Hive, in
//! throwaway homes: an old link file → `hive connect` → `hive link` →
//! `hive status` / `hive sync` / `hive connect` again.
//!
//! The fake binds an identity to the vault of its first resolve, like
//! Hive (`vault_bindings`, first binding wins).
#![cfg(feature = "http-server")]

use crate::support::fake_hive::{FakeHive, FakeOptions};
use lean_ctx_hive::{HiveConfig, HiveStore};
use lean_ctx_protocol::hive::status::HiveCliStatusV1;
use lean_ctx_protocol::hive::vault::stable_identity;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
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

    /// `lean-ctx hive <args>` with `dir` as working directory and project root.
    fn hive(&self, dir: &Path, args: &[&str]) -> Output {
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
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("hive")
            .args(args)
            .output()
            .unwrap()
    }

    /// Connect from `dir` (`--yes` records the sharing answer). A first
    /// share that waits for the person is approved, as they would in their
    /// own terminal (`hive consent approve`), and connect runs again; the
    /// output of both runs is returned.
    fn connect(&self, fake: &FakeHive, dir: &Path) -> String {
        let run = |extra: &[&str]| {
            let mut args = vec!["connect", "--url", &fake.url, "--no-browser"];
            args.extend_from_slice(extra);
            let out = self.hive(dir, &args);
            assert!(out.status.success(), "{out:?}");
            said(&out)
        };
        let first = run(&["--label", "box", "--yes"]);
        if !first.contains("until you confirm") || !self.approve_pending(fake) {
            return first;
        }
        format!("{first}{}", run(&[]))
    }

    /// `hive sync` from `dir`; a first share that waits for the person is
    /// approved and sync runs again. The output of both runs is returned.
    fn sync(&self, fake: &FakeHive, dir: &Path) -> String {
        let first = said(&self.hive(dir, &["sync"]));
        if !first.contains("consent_required") || !self.approve_pending(fake) {
            return first;
        }
        format!("{first}{}", said(&self.hive(dir, &["sync"])))
    }

    fn store(&self) -> HiveStore {
        HiveStore::at(
            self.home.path().join("config/hive"),
            self.home.path().join("data/hive"),
        )
    }

    /// Records `record` as this machine's only vault resolution.
    fn write_projects(&self, record: &Value) {
        let dir = self.home.path().join("data/hive");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("projects.json"),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
    }

    /// Approves every pending consent request; whether there was one.
    fn approve_pending(&self, fake: &FakeHive) -> bool {
        use lean_ctx_hive::consent::broker::Approval;
        use lean_ctx_hive::consent::{Broker, State, Trust};
        let store = self.store();
        let config = HiveConfig {
            url: Some(fake.url.clone()),
            root_key: Some(FakeHive::root_public_key_b64()),
            ..HiveConfig::default()
        };
        let broker = Broker::open(&store, Trust::load(&store, &config));
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
        let mut any = false;
        for listed in broker.list().unwrap() {
            if listed.record.state == State::Pending {
                broker
                    .approve(
                        &listed.record.consent_id,
                        Approval::TtyOnly { approved_at: now },
                        now,
                    )
                    .unwrap();
                any = true;
            }
        }
        any
    }

    fn projects(&self) -> Value {
        std::fs::read(self.home.path().join("data/hive/projects.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| json!({"projects": {}}))
    }

    /// `ns/name` and vault id this machine recorded for `root`.
    fn recorded(&self, root: &Path) -> Option<(String, String)> {
        let root = root.to_string_lossy();
        self.projects()["projects"]
            .as_object()?
            .values()
            .find(|r| {
                r["roots"]
                    .as_array()
                    .is_some_and(|roots| roots.iter().any(|x| x.as_str() == Some(&root)))
            })
            .map(|r| {
                (
                    format!(
                        "{}/{}",
                        r["ns"].as_str().unwrap(),
                        r["name"].as_str().unwrap()
                    ),
                    r["project_id"].as_str().unwrap().to_string(),
                )
            })
    }

    fn status_json(&self, dir: &Path) -> HiveCliStatusV1 {
        let out = self.hive(dir, &["status", "--json"]);
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {out:?}"))
    }
}

/// A `lean-ctx mcp` session on stdio in `dir`, as an agent host runs it.
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
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
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
            &json!({"protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "hive-suite", "version": "1"}}),
        );
        session.write(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        session
    }

    fn write(&mut self, message: &Value) {
        use std::io::Write as _;
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: &Value) -> Value {
        use std::io::BufRead as _;
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "mcp closed");
            let reply: Value = serde_json::from_str(&line).unwrap();
            if reply["id"] == id {
                return reply;
            }
        }
    }

    fn call(&mut self, tool: &str, arguments: &Value) -> String {
        let reply = self.request("tools/call", &json!({"name": tool, "arguments": arguments}));
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

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const ORIGIN: &str = "git@github.com:yves/leanctx-hive.git";

/// A git checkout of [`ORIGIN`] named `name` in `machine`'s home.
fn checkout(machine: &Machine, name: &str) -> PathBuf {
    let root = machine.home.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["remote", "add", "origin", ORIGIN]);
    root
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A committed link file (as a teammate's commit or an older engine
/// wrote it): no `hive link` on this machine.
fn link_file(root: &Path, vault: &str) {
    lean_ctx_hive::link::link(root, vault).unwrap();
}

fn fake() -> FakeHive {
    with_repos(FakeHive::start())
}

/// A Hive that speaks C0 2.x (instance card), optionally with the match
/// route another Instance's binding is found by.
fn fake_2x(vault_match: bool) -> FakeHive {
    with_repos(FakeHive::start_with(FakeOptions {
        instance_card: true,
        vault_match,
        ..FakeOptions::default()
    }))
}

fn with_repos(fake: FakeHive) -> FakeHive {
    fake.create_web_repo("yves", "hive", true);
    fake.create_web_repo("yves", "leanctx-hive", true);
    fake
}

/// Requests into the vault `vault_id` after the first `skip` requests.
fn into_vault(fake: &FakeHive, vault_id: &str, skip: usize) -> Vec<String> {
    let prefix = format!("/v1/vaults/{vault_id}/");
    fake.requests()
        .into_iter()
        .skip(skip)
        .filter(|r| r.path.starts_with(&prefix))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

/// Acceptance (1) and (2): old link file → connect → link. Hive keeps the
/// first binding (yves/hive), so the link cannot move it: status says so,
/// sync and the next connect refuse plainly, and nothing goes into
/// yves/hive any more.
#[test]
fn link_after_connect_never_keeps_syncing_the_old_vault() {
    let fake = fake();
    let pounce = Machine::new();
    let repo = checkout(&pounce, "leanctx-hive");
    link_file(&repo, "yves/hive");

    let out = pounce.connect(&fake, &repo);
    assert!(
        out.contains("This project syncs with the vault yves/hive"),
        "{out}"
    );
    let (_, old_id) = pounce.recorded(&repo).expect("resolved");

    let link = pounce.hive(&repo, &["link", "yves/leanctx-hive"]);
    assert!(link.status.success(), "{link:?}");
    let out = said(&link);
    assert!(out.contains("resolved"), "{out}");
    assert!(out.contains("That resolution is dropped"), "{out}");
    assert!(out.contains("lean-ctx hive connect"), "{out}");
    assert_eq!(pounce.recorded(&repo), None, "the old vault is not used");
    let after_link = fake.requests().len();

    let status = said(&pounce.hive(&repo, &["status"]));
    assert!(!status.contains("→ yves/hive (shared)"), "{status}");

    let sync = pounce.sync(&fake, &repo);
    assert!(sync.contains("vault_mismatch"), "{sync}");
    assert!(
        sync.contains("yves/hive") && sync.contains("yves/leanctx-hive"),
        "{sync}"
    );
    assert_eq!(
        pounce.recorded(&repo),
        None,
        "a refused answer is not recorded"
    );

    let again = pounce.connect(&fake, &repo);
    assert!(again.contains("vault_mismatch"), "{again}");
    assert!(!again.contains("syncs with the vault yves/hive"), "{again}");
    assert_eq!(into_vault(&fake, &old_id, after_link), Vec::<String>::new());
}

/// Acceptance (2) and the lead's root cause: a stale `projects.json` entry
/// (yves/hive) and a newer link file (yves/leanctx-hive, from a pulled
/// commit). Status flags it, plain and `--json`; sync resolves the linked
/// vault and never touches the old one.
#[test]
fn a_stale_resolution_is_flagged_and_sync_resolves_the_linked_vault() {
    let fake = fake();
    let pounce = Machine::new();
    let home = pounce.home.path().to_path_buf();
    pounce.connect(&fake, &home);
    let repo = checkout(&pounce, "leanctx-hive");
    let identity = lean_ctx_hive::vault::project_identity(&repo).unwrap();
    let stale_id = "0dfac5e1b2c3d4e5f60718293a4b5c6d";
    let stale = json!({"projects": {stable_identity(&identity): {
        "project_id": stale_id, "ns": "yves", "name": "hive", "sealed": false,
        "roots": [repo.to_string_lossy()],
    }}});
    std::fs::create_dir_all(home.join("data/hive")).unwrap();
    std::fs::write(
        home.join("data/hive/projects.json"),
        serde_json::to_vec(&stale).unwrap(),
    )
    .unwrap();
    link_file(&repo, "yves/leanctx-hive");

    let status = said(&pounce.hive(&repo, &["status"]));
    assert!(status.contains("MISMATCH"), "{status}");
    assert!(!status.contains("(shared)"), "{status}");
    let json = pounce.status_json(&repo);
    let mismatch = json.vault_mismatch.expect("vault_mismatch");
    assert_eq!(mismatch.resolved, "yves/hive");
    assert_eq!(mismatch.linked.as_deref(), Some("yves/leanctx-hive"));
    assert_eq!(mismatch.reason, "link");
    assert_eq!(mismatch.project_root, repo.to_string_lossy());

    let sync = pounce.hive(&repo, &["sync"]);
    assert!(sync.status.success(), "{sync:?}");
    let (vault, _) = pounce.recorded(&repo).expect("resolved again");
    assert_eq!(vault, "yves/leanctx-hive");
    assert_eq!(into_vault(&fake, stale_id, 0), Vec::<String>::new());
    let resolve = fake
        .requests()
        .into_iter()
        .rfind(|r| r.path == "/v1/vaults/resolve")
        .expect("resolved");
    assert!(resolve.body.contains("leanctx-hive"), "{}", resolve.body);

    assert!(pounce.status_json(&repo).vault_mismatch.is_none());
    let status = said(&pounce.hive(&repo, &["status"]));
    assert!(status.contains("→ yves/leanctx-hive (shared)"), "{status}");
}

/// Acceptance (3) and the deeper root cause: two checkouts of one origin
/// (Mac, pounce), both linked, are one repository in one vault, also when
/// pounce still has the random `.lean-ctx-id` an earlier `hive link` wrote
/// and a record under that identity. While pounce's old link file names
/// another vault, its connect says so instead of splitting the repository.
#[test]
fn two_linked_checkouts_of_one_origin_resolve_to_one_vault() {
    let fake = fake();
    let mac = Machine::new();
    let mac_repo = checkout(&mac, "leanctx-hive");
    let link = mac.hive(&mac_repo, &["link", "yves/leanctx-hive"]);
    assert!(link.status.success(), "{link:?}");
    assert!(
        !mac_repo.join(".lean-ctx-id").exists(),
        "an origin needs no id"
    );
    let out = mac.connect(&fake, &mac_repo);
    assert!(
        out.contains("syncs with the vault yves/leanctx-hive"),
        "{out}"
    );
    let (_, mac_vault) = mac.recorded(&mac_repo).unwrap();

    let pounce = Machine::new();
    let repo = checkout(&pounce, "leanctx-hive");
    let random = "0dfac5e1b2c3d4e5f60718293a4b5c6d";
    std::fs::write(repo.join(".lean-ctx-id"), format!("{random}\n")).unwrap();
    link_file(&repo, "yves/hive");
    let home = pounce.home.path().to_path_buf();
    pounce.connect(&fake, &home);
    let old = json!({"projects": {stable_identity(&format!("explicit:{random}")): {
        "project_id": "1f2e3d4c5b6a79881f2e3d4c5b6a7988", "ns": "yves", "name": "hive",
        "sealed": false, "roots": [repo.to_string_lossy()],
    }}});
    std::fs::create_dir_all(home.join("data/hive")).unwrap();
    std::fs::write(
        home.join("data/hive/projects.json"),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();

    // Old link file → connect: Hive has the repository in yves/leanctx-hive
    // (the Mac's Instance bound it), so pounce is told, never split.
    let out = pounce.connect(&fake, &repo);
    assert!(out.contains("resolving it again"), "{out}");
    assert!(out.contains("vault_mismatch"), "{out}");
    assert!(out.contains("another Instance"), "{out}");
    assert!(!out.contains("syncs with the vault yves/hive"), "{out}");
    assert_eq!(pounce.recorded(&repo), None);

    // → link → connect → connect again: the Mac's vault.
    let link = pounce.hive(&repo, &["link", "yves/leanctx-hive"]);
    assert!(link.status.success(), "{link:?}");
    for _ in 0..2 {
        let out = pounce.connect(&fake, &repo);
        assert!(
            out.contains("syncs with the vault yves/leanctx-hive"),
            "{out}"
        );
        assert_eq!(
            pounce.recorded(&repo),
            Some(("yves/leanctx-hive".to_string(), mac_vault.clone()))
        );
    }
    assert!(pounce.status_json(&repo).vault_mismatch.is_none());
    let sync = pounce.hive(&repo, &["sync"]);
    assert!(sync.status.success(), "{sync:?}");
    assert_eq!(
        pounce.recorded(&repo),
        Some(("yves/leanctx-hive".to_string(), mac_vault))
    );
}

/// Review F1 (the #66-68 pattern): a `git pull` changed the link file and
/// nothing has resolved the project again (no `hive link`, connect or
/// sync). Status shows the mismatch, and every write refuses with
/// `vault_mismatch` instead of going into the vault the checkout left: a
/// task claim, a doc import and a bus message.
#[test]
fn writes_after_a_pulled_link_change_never_reach_the_old_vault() {
    let fake = fake_2x(false);
    let pounce = Machine::new();
    let repo = checkout(&pounce, "leanctx-hive");
    link_file(&repo, "yves/hive");
    std::fs::write(repo.join("README.md"), "# leanctx-hive\n\nHow it works.\n").unwrap();
    git(&repo, &["add", "README.md"]);
    let out = pounce.connect(&fake, &repo);
    assert!(out.contains("syncs with the vault yves/hive"), "{out}");
    let (_, old_id) = pounce.recorded(&repo).expect("resolved");

    // The pulled commit names the other vault; projects.json still has the
    // old resolution.
    link_file(&repo, "yves/leanctx-hive");
    // Doc pages need Hive-Protocol 2.2; the fake answers 2.0.
    lean_ctx_hive::bus::relay_v2::record_protocol_minor(&pounce.store(), true, Some(2)).unwrap();
    let skip = fake.requests().len();
    let status = said(&pounce.hive(&repo, &["status"]));
    assert!(status.contains("MISMATCH"), "{status}");

    let claim = said(&pounce.hive(&repo, &["tasks", "claim", "1", "--agent", "coder"]));
    assert!(claim.contains("vault_mismatch"), "task: {claim}");
    let doc = said(&pounce.hive(&repo, &["docs", "import", "--markdown", "README.md"]));
    assert!(doc.contains("vault_mismatch"), "doc: {doc}");
    let mut host = McpSession::start(&pounce, &repo);
    let registered = host.call(
        "ctx_agent",
        &json!({"action": "register", "agent_type": "claude", "role": "coder"}),
    );
    assert!(registered.contains("Agent registered"), "{registered}");
    let sent = host.call(
        "ctx_agent",
        &json!({"action": "send", "type": "finding", "message": "the parser drops BOMs"}),
    );
    assert!(sent.contains("vault_mismatch"), "bus: {sent}");
    drop(host);

    assert_eq!(into_vault(&fake, &old_id, skip), Vec::<String>::new());
    assert_eq!(
        pounce.recorded(&repo).map(|(vault, _)| vault).as_deref(),
        Some("yves/hive"),
        "refused, not rewritten: connect or sync resolves it again"
    );
}

/// Acceptance (3): Hive bound the repository to yves/leanctx-hive (the
/// Mac's Instance resolved it first) while pounce, without a link file,
/// syncs it with yves/hive. Connect says so instead of splitting it
/// silently.
#[test]
fn connect_warns_when_another_instance_bound_the_repository_elsewhere() {
    let fake = fake_2x(true);
    let mac = Machine::new();
    let mac_repo = checkout(&mac, "leanctx-hive");
    assert!(
        mac.hive(&mac_repo, &["link", "yves/leanctx-hive"])
            .status
            .success()
    );
    let out = mac.connect(&fake, &mac_repo);
    assert!(
        out.contains("syncs with the vault yves/leanctx-hive"),
        "{out}"
    );
    assert!(!out.contains("Warning: Hive binds"), "{out}");

    let pounce = Machine::new();
    let repo = checkout(&pounce, "leanctx-hive");
    pounce.connect(&fake, pounce.home.path());
    let identity = lean_ctx_hive::vault::project_identity(&repo).unwrap();
    pounce.write_projects(&json!({"projects": {stable_identity(&identity): {
        "project_id": "1f2e3d4c5b6a79881f2e3d4c5b6a7988", "ns": "yves", "name": "hive",
        "sealed": false, "roots": [repo.to_string_lossy()],
    }}}));

    let out = pounce.connect(&fake, &repo);
    assert!(out.contains("syncs with the vault yves/hive"), "{out}");
    assert!(
        out.contains("Warning: Hive binds this repository to the vault yves/leanctx-hive"),
        "{out}"
    );
    assert!(out.contains("another Instance"), "{out}");
    assert!(
        out.contains("lean-ctx hive link yves/leanctx-hive"),
        "{out}"
    );
}

/// Review F1 (round 2): the daemon's background sync. A stale record and a
/// pulled link file, then one pass of the background thread (`sync::auto`,
/// as the daemon runs it): it neither pulls from nor pushes to the old
/// vault, and the stale record is dropped before anything is planned.
///
/// The status the thread writes after its first pass is the evidence: a
/// pass that planned the old vault leaves a pass time and a row for it
/// (with whatever error the sync met), whether or not a request got out.
#[test]
fn the_background_sync_never_touches_the_vault_a_pulled_link_left() {
    use lean_ctx_hive::sync::auto;
    let fake = fake_2x(false);
    let pounce = Machine::new();
    let repo = checkout(&pounce, "leanctx-hive");
    link_file(&repo, "yves/hive");
    let out = pounce.connect(&fake, &repo);
    assert!(out.contains("syncs with the vault yves/hive"), "{out}");
    let (_, old_id) = pounce.recorded(&repo).expect("resolved");

    link_file(&repo, "yves/leanctx-hive");
    let skip = fake.requests().len();
    let store = pounce.store();
    let data = store.data_dir().to_path_buf();
    let config = HiveConfig {
        url: Some(fake.url.clone()),
        root_key: Some(FakeHive::root_public_key_b64()),
        ..HiveConfig::default()
    };
    // Its first pass syncs every vault never tried at once.
    auto::spawn(store, config);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    // Its first status write follows its first pass.
    let first = loop {
        let status = auto::load_status(&data);
        if status.heartbeat_at.is_some() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline, "no background pass");
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    auto::stop();
    let old_place = lean_ctx_hive::sync::place::place_id(&old_id);
    assert!(
        first.vaults.iter().all(|row| row.vault_id != old_place),
        "the first pass planned the old vault: {first:?}"
    );
    assert_eq!(first.last_pass_at, None, "nothing was due: {first:?}");
    // The thread writes its last status (no heartbeat) when it ends.
    while auto::load_status(&data).heartbeat_at.is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the thread did not stop"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    assert_eq!(into_vault(&fake, &old_id, skip), Vec::<String>::new());
    assert_eq!(pounce.recorded(&repo), None, "the stale record is dropped");
}
