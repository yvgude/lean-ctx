// SPDX-License-Identifier: Apache-2.0
use super::*;

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

fn parse(args: &[&str]) -> Result<Args, Failure> {
    Args::parse(&strings(args))
}

#[test]
fn arguments_parse_values_flags_and_the_task() {
    let args = parse(&[
        "--cloud",
        "fix the flaky test",
        "--computer=dev",
        "--agent",
        "codex",
        "--model-access",
        "balance",
        "--max-minutes",
        "30",
        "--yes",
    ])
    .expect("parses");
    assert_eq!(args.positional, ["fix the flaky test"]);
    assert_eq!(args.value("--computer"), Some("dev"));
    assert_eq!(args.agent().expect("agent"), "codex");
    assert_eq!(args.model_access(true).expect("access"), "balance");
    assert_eq!(args.max_minutes().expect("minutes"), Some(30));
    assert!(args.has("--cloud") && args.has("--yes"));

    let forced = parse(&["--", "list"]).expect("parses");
    assert!(forced.task_only);
    assert_eq!(forced.positional, ["list"]);

    assert!(
        parse(&["t", "--push"]).is_err(),
        "unknown flags are refused"
    );
    assert!(
        parse(&["t", "--agent"]).is_err(),
        "a value flag needs its value"
    );
    assert!(parse(&["t", "--agent", "a", "--agent", "b"]).is_err());
    assert!(parse(&["t", "--yes=1"]).is_err());
}

#[test]
fn the_person_chooses_model_access_for_a_cloud_run() {
    let args = parse(&["--cloud", "t"]).expect("parses");
    let failure = args.model_access(true).expect_err("must be chosen");
    assert_eq!(failure.code, Code::InvalidArgument);
    assert_eq!(args.model_access(false).expect("local default"), "own");
    let bad = parse(&["t", "--model-access", "free"]).expect("parses");
    assert!(bad.model_access(false).is_err());
    assert!(
        parse(&["t", "--agent", "gpt"])
            .expect("parses")
            .agent()
            .is_err()
    );
    assert!(
        parse(&["t", "--max-minutes", "0"])
            .expect("parses")
            .max_minutes()
            .is_err()
    );
    assert!(
        parse(&["t", "--max-minutes", "1441"])
            .expect("parses")
            .max_minutes()
            .is_err()
    );
}

#[test]
fn run_ids_are_checked_before_any_use() {
    assert!(
        parse(&["status", "run_0123456789abcdef"])
            .expect("parses")
            .id()
            .is_ok()
    );
    assert!(parse(&["status", "../x"]).expect("parses").id().is_err());
    assert!(parse(&["status"]).expect("parses").id().is_err());
}

#[test]
fn tasks_are_bounded_and_not_empty() {
    assert!(check_task("  \n".into()).is_err());
    assert!(check_task("x".repeat(store::MAX_TASK_BYTES + 1)).is_err());
    assert_eq!(check_task("fix it".into()).expect("task"), "fix it");
}

#[test]
fn agents_start_headless_with_the_task_on_stdin() {
    let (program, args) = agent_command("claude");
    assert_eq!(program, "claude");
    assert!(args.contains(&"-p") && args.contains(&"stream-json"));
    let (program, args) = agent_command("codex");
    assert_eq!(program, "codex");
    assert_eq!(args.last(), Some(&"-"), "codex reads the task from stdin");
}

#[test]
fn balance_access_points_the_agent_at_the_gateway() {
    let key = lean_ctx_cloud::gateway::GatewayKey {
        base_url: "https://gateway.example".into(),
        key: "k".into(),
        expires_at: "2026-11-19T12:00:00Z".into(),
    };
    let claude: Vec<&str> = gateway_env("claude", &key)
        .iter()
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(claude, ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN"]);
    let codex = gateway_env("codex", &key);
    assert_eq!(
        codex[0],
        ("OPENAI_BASE_URL", "https://gateway.example".to_owned())
    );
    assert_eq!(codex[1].0, "OPENAI_API_KEY");
}

#[test]
fn tool_calls_are_counted_from_the_agent_output() {
    let claude = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","name":"Edit"},{"type":"tool_use","name":"Bash"}]}}"#;
    assert_eq!(tool_calls_in("claude", claude), 2);
    assert_eq!(
        tool_calls_in(
            "claude",
            r#"{"type":"user","message":{"content":[{"type":"tool_use"}]}}"#
        ),
        0
    );
    assert_eq!(tool_calls_in("claude", "not json"), 0);
    let done = r#"{"type":"item.completed","item":{"type":"command_execution","command":"ls"}}"#;
    let begun = r#"{"type":"item.started","item":{"type":"command_execution","command":"ls"}}"#;
    assert_eq!(tool_calls_in("codex", done), 1);
    assert_eq!(tool_calls_in("codex", begun), 0, "a call counts once");
}

#[test]
fn events_read_as_lines() {
    let commit = json!({ "seq": 3, "type": "commit", "sha": "0123456789abcdef0123" });
    assert_eq!(event_line(&commit), "[3] commit 0123456789ab");
    let end = json!({ "seq": 4, "type": "stopped", "commits": 1, "stop_reason": "user", "exit_code": null });
    assert_eq!(event_line(&end), "[4] stopped 1 commit(s), reason user");
    assert!(is_final_event(&end));
    assert!(!is_final_event(&commit));
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let mut all = vec!["-c", "user.name=t", "-c", "user.email=t@example.invalid"];
    all.extend_from_slice(args);
    git(dir, &all).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn repository() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir");
    git_ok(&repo, &["init", "--quiet"]);
    std::fs::write(repo.join("a.txt"), "one\n").expect("write");
    git_ok(&repo, &["add", "a.txt"]);
    git_ok(&repo, &["commit", "--quiet", "-m", "base"]);
    (dir, repo)
}

#[test]
fn include_dirty_starts_from_the_uncommitted_changes() {
    let (_dir, repo) = repository();
    let head = git_ok(&repo, &["rev-parse", "HEAD"]);
    let clean = parse(&["t", "--include-dirty"]).expect("parses");
    assert_eq!(
        base_commit(&repo, &clean).expect("base"),
        head,
        "nothing dirty: HEAD"
    );
    std::fs::write(repo.join("a.txt"), "two\n").expect("write");
    let dirty = base_commit(&repo, &clean).expect("base");
    assert_ne!(dirty, head);
    assert_eq!(git_ok(&repo, &["show", &format!("{dirty}:a.txt")]), "two");
    assert_eq!(
        git_ok(&repo, &["status", "--porcelain"]),
        "M a.txt",
        "the work tree is untouched"
    );
    let with_base = parse(&["t", "--include-dirty", "--base", "HEAD"]).expect("parses");
    assert!(base_commit(&repo, &with_base).is_err());
    let bad = parse(&["t", "--base", "no-such-ref"]).expect("parses");
    assert!(base_commit(&repo, &bad).is_err());
}

#[test]
fn a_run_ends_with_its_commits_and_the_agent_leftovers() {
    let (dir, repo) = repository();
    let base = git_ok(&repo, &["rev-parse", "HEAD"]);
    let store = Store::new(dir.path().join("runs"));
    let id = "run_00000000000000aa";
    let worktree = dir.path().join("wt");
    let worktree_text = worktree.display().to_string();
    git_ok(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &store::branch_of(id),
            &worktree_text,
            &base,
        ],
    );
    std::fs::write(worktree.join("b.txt"), "agent\n").expect("write");
    git_ok(&worktree, &["add", "b.txt"]);
    git_ok(&worktree, &["commit", "--quiet", "-m", "agent commit"]);
    std::fs::write(worktree.join("c.txt"), "left over\n").expect("write");
    let mut record = RunRecord {
        schema: store::RECORD_SCHEMA.into(),
        id: id.into(),
        place: store::PLACE_LOCAL.into(),
        computer: None,
        project_root: repo.display().to_string(),
        agent: "claude".into(),
        model_access: "own".into(),
        state: store::STATE_RUNNING.into(),
        started_at: "2026-11-19T10:00:00Z".into(),
        ended_at: None,
        branch: store::branch_of(id),
        base: base.clone(),
        commits: Vec::new(),
        tool_calls: 0,
        exit_code: None,
        stop_reason: None,
        worktree: Some(worktree_text),
        pid: None,
        unit: None,
    };
    store.create(&record, "t").expect("create");
    record.commits = collect_commits(&store, &worktree, &record);
    assert_eq!(
        record.commits.len(),
        2,
        "the agent's commit and its leftovers"
    );
    assert_eq!(git_ok(&worktree, &["status", "--porcelain"]), "");
    end(&store, &mut record, store::STATE_FINISHED, None, Some(0));
    let events = store.events(id, 0, 10).expect("events");
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(kinds, ["commit", "commit", "finished"]);
    assert_eq!(events[2]["commits"], 2);
    let saved = store.load(id).expect("load").expect("record");
    assert_eq!(saved.state, store::STATE_FINISHED);
    assert!(saved.ended_at.is_some());
    // The run branch is in the project: the worktree shares its refs.
    let tip = git_ok(&repo, &["rev-parse", &store::branch_of(id)]);
    assert_eq!(Some(&tip), record.commits.last());
}

#[cfg(unix)]
#[test]
fn a_group_that_ignores_sigterm_is_killed_after_the_grace() {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    let mut child = Command::new("sh")
        .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    // Let the shell install its trap before the signal.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    terminate_group(child.id(), std::time::Duration::from_millis(500));
    let status = child.wait().expect("wait");
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    assert!(started.elapsed() >= std::time::Duration::from_millis(500));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[cfg(unix)]
#[test]
fn a_group_that_honours_sigterm_is_not_killed() {
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    let mut child = Command::new("sleep")
        .arg("60")
        .process_group(0)
        .stdin(Stdio::null())
        .spawn()
        .expect("spawn");
    terminate_group(child.id(), std::time::Duration::from_secs(10));
    let status = child.wait().expect("wait");
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
}

#[test]
fn a_used_up_balance_names_a_top_up_only_when_one_is_on_offer() {
    use lean_ctx_cloud::gateway::GatewayAnswer;
    let trial = gateway_refusal(GatewayAnswer::Stopped {
        top_up: false,
        retry_after_s: None,
    })
    .expect_err("stopped");
    assert!(trial.contains("--model-access own"), "{trial}");
    assert!(!trial.to_lowercase().contains("top"), "{trial}");
    let paid = gateway_refusal(GatewayAnswer::Stopped {
        top_up: true,
        retry_after_s: None,
    })
    .expect_err("stopped");
    assert!(
        paid.contains("top it up") && paid.contains("--model-access own"),
        "{paid}"
    );
    let other = gateway_refusal(GatewayAnswer::Forbidden).expect_err("forbidden");
    assert!(other.contains("your own"), "{other}");
    assert!(NOT_CONNECTED_AS_YOU.contains("as you"));
}

mod cloud_side {
    use super::super::cloud::*;
    use super::*;

    fn computer(hostname: &str) -> ssh_types::Computer {
        ssh_types::Computer {
            name: "dev".into(),
            hostname: hostname.into(),
            user: "leanctx".into(),
            port: 2222,
            identity_file: Some("/home/p/.ssh/id ed25519".into()),
            host_keys: Vec::new(),
        }
    }

    use crate::cli::cloud_cmd::cloud_computer_cmd::ssh as ssh_types;

    #[test]
    fn the_repository_hash_is_stable_and_safe() {
        let hash = repo_hash(Path::new("/work/project"));
        assert_eq!(hash, repo_hash(Path::new("/work/project")));
        assert_ne!(hash, repo_hash(Path::new("/work/other")));
        assert_eq!(hash.len(), 16);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            base_ref("run_00000000000000aa"),
            "refs/leanctx/runs/run_00000000000000aa/base"
        );
    }

    #[test]
    fn git_reaches_the_computer_with_the_pin_enforced() {
        let known = Path::new("/home/p/.ssh/leanctx/known_hosts");
        let command = git_ssh_command(&computer("203.0.113.7"), known);
        assert!(
            command.starts_with("ssh '-o' 'StrictHostKeyChecking=yes'"),
            "{command}"
        );
        assert!(command.contains("'HostKeyAlias=leanctx-dev'"));
        assert!(command.contains("'ForwardAgent=no'"));
        assert!(
            command.contains("'/home/p/.ssh/id ed25519'"),
            "quoted: {command}"
        );
        assert!(!command.contains("203.0.113.7"), "git names the host");
        assert!(!command.contains("'-p'"), "git names the port");
        assert_eq!(
            repo_url(&computer("203.0.113.7"), "00112233aabbccdd"),
            "ssh://leanctx@203.0.113.7:2222/~/.leanctx/repos/00112233aabbccdd.git"
        );
        assert_eq!(
            repo_url(&computer("2001:db8::7"), "00112233aabbccdd"),
            "ssh://leanctx@[2001:db8::7]:2222/~/.leanctx/repos/00112233aabbccdd.git"
        );
    }

    #[test]
    fn the_price_is_shown_from_the_cloud_terms() {
        let details = json!({ "billing": "billed", "usd_per_hour": 0.09, "topup_usd": 12.4 });
        let terms = lean_ctx_cloud::computers::terms(details.as_object());
        assert_eq!(
            price_line(&terms),
            "Price: billed from the top-up, $0.09 per hour from the first minute, top-up $12.40 left."
        );
        let unknown = lean_ctx_cloud::computers::terms(None);
        assert_eq!(price_line(&unknown), "Price: price per hour not reported.");
    }

    #[test]
    fn the_terms_reach_the_error_document() {
        let details = json!({ "billing": "billed", "usd_per_hour": 0.09, "topup_usd": 12.4 });
        let terms = lean_ctx_cloud::computers::terms(details.as_object());
        let failure = Failure::new(Code::ConfirmationRequired, "confirm")
            .with_details(terms_details(&terms, Some("ok")));
        let doc: Value = serde_json::from_str(&local_read::error_document(&failure)).expect("json");
        assert_eq!(doc["kind"], "error");
        assert_eq!(doc["error"]["code"], "confirmation_required");
        assert_eq!(doc["error"]["details"]["billing"], "billed");
        assert_eq!(doc["error"]["details"]["usd_per_hour"], 0.09);
        assert_eq!(doc["error"]["details"]["topup_usd"], 12.4);
        assert_eq!(doc["error"]["details"]["topup_state"], "ok");
        for gone in ["free_hours_left", "balance_usd"] {
            assert!(doc["error"]["details"].get(gone).is_none(), "{gone}");
        }
    }

    /// A loopback LeanCTX Cloud: answers each request in turn with the
    /// next of `answers` (`500` once they run out) and records every request
    /// it receives, complete, before it answers.
    fn loopback_cloud(
        answers: Vec<(u16, String)>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            let mut answers = answers.into_iter();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stream.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                record
                    .lock()
                    .expect("record")
                    .push(String::from_utf8_lossy(&buf).into_owned());
                let (status, body) = answers.next().unwrap_or((500, "{}".into()));
                let answer = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nCloud-Protocol: 2.2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        (origin, seen)
    }

    fn listed_row() -> Map<String, Value> {
        json!({ "id": "cmp_1", "name": "dev", "state": "stopped",
                "price": { "usd_per_hour": 0.09 },
                "balance": { "usd": 12.4, "state": "ok", "free_hours_left": 300.0 } })
        .as_object()
        .expect("row")
        .clone()
    }

    #[test]
    fn without_yes_nothing_is_sent_and_the_terms_come_back() {
        let (origin, seen) = loopback_cloud(vec![(
            200,
            r#"{"computer":{"id":"cmp_1","name":"dev","state":"starting"}}"#.into(),
        )]);
        let client = lean_ctx_cloud::transport::CloudClient::new(&origin).expect("client");
        let args = parse(&["--cloud", "fix it", "--json"]).expect("args");
        let failure = start_paid(
            &lean_ctx_cloud::computers::on_client(&client),
            &listed_row(),
            "req_12345678",
            &args,
            true,
        )
        .expect_err("refused locally");
        assert_eq!(failure.code, Code::ConfirmationRequired);
        assert!(
            seen.lock().expect("seen").is_empty(),
            "no request reached the cloud"
        );
        let doc: Value = serde_json::from_str(&local_read::error_document(&failure)).expect("json");
        assert_eq!(doc["error"]["details"]["billing"], "billed");
        assert_eq!(doc["error"]["details"]["usd_per_hour"], 0.09);
        assert_eq!(doc["error"]["details"]["topup_usd"], 12.4);
        assert_eq!(doc["error"]["details"]["topup_state"], "ok");
        assert!(
            doc["error"]["details"].get("free_hours_left").is_none(),
            "a run is billed even while a team has free hours"
        );
    }

    #[test]
    fn with_yes_one_confirmed_start_is_sent() {
        let (origin, seen) = loopback_cloud(vec![(
            200,
            r#"{"computer":{"id":"cmp_1","name":"dev","state":"starting"}}"#.into(),
        )]);
        let client = lean_ctx_cloud::transport::CloudClient::new(&origin).expect("client");
        let args = parse(&["--cloud", "fix it", "--json", "--yes"]).expect("args");
        let row = start_paid(
            &lean_ctx_cloud::computers::on_client(&client),
            &listed_row(),
            "req_12345678",
            &args,
            true,
        )
        .expect("started");
        assert_eq!(row["state"], "starting");
        let seen = seen.lock().expect("seen");
        assert_eq!(seen.len(), 1, "exactly one request");
        assert!(
            seen[0].starts_with("POST /v1/computers/cmp_1/start "),
            "{}",
            seen[0]
        );
        let body: Value =
            serde_json::from_str(&seen[0][seen[0].find("\r\n\r\n").expect("body") + 4..])
                .expect("json");
        assert_eq!(
            body,
            json!({"request_id": "req_12345678", "purpose": "run", "confirm": true})
        );
    }

    #[test]
    fn a_428_to_a_confirmed_start_is_an_error_not_a_second_ask() {
        let refusal = r#"{"error":{"code":"confirmation_required","message":"confirm","details":{"billing":"billed","usd_per_hour":0.12}}}"#;
        let (origin, seen) = loopback_cloud(vec![(428, refusal.into())]);
        let client = lean_ctx_cloud::transport::CloudClient::new(&origin).expect("client");
        let args = parse(&["--cloud", "fix it", "--yes"]).expect("args");
        let failure = start_paid(
            &lean_ctx_cloud::computers::on_client(&client),
            &listed_row(),
            "req_12345678",
            &args,
            true,
        )
        .expect_err("refused");
        assert_eq!(failure.code, Code::ConfirmationRequired);
        assert_eq!(seen.lock().expect("seen").len(), 1, "never sent again");
        let doc: Value = serde_json::from_str(&local_read::error_document(&failure)).expect("json");
        assert_eq!(doc["error"]["details"]["usd_per_hour"], 0.12);
    }

    #[test]
    fn computers_not_available_end_the_run_before_any_start() {
        for (status, body) in [
            (200, r#"{"available":false,"computers":[]}"#),
            (
                503,
                r#"{"error":{"code":"computer_not_available","message":"raw cloud text"}}"#,
            ),
        ] {
            let (origin, seen) = loopback_cloud(vec![(status, body.into())]);
            let client = lean_ctx_cloud::transport::CloudClient::new(&origin).expect("client");
            let args = parse(&["--cloud", "fix it", "--yes", "--json"]).expect("args");
            let instance = lean_ctx_cloud::state::InstanceRecord {
                instance_id: "inst_1".into(),
                org_id: "org_acme".into(),
                org_name: "acme".into(),
                label: "laptop".into(),
                url: origin.clone(),
                org_salt_id: "salt".into(),
                enrolled_at: "2026-11-01T00:00:00Z".into(),
                web_url: None,
                org_kind: Some("personal".into()),
                company_name: None,
            };
            let failure = chosen_row(
                &lean_ctx_cloud::computers::on_client(&client),
                &args,
                &instance,
            )
            .expect_err("not available");
            assert_eq!(failure.code, Code::ComputerNotAvailable, "{body}");
            assert_eq!(failure.message, "LeanCTX computers are not available yet.");
            let seen = seen.lock().expect("seen");
            assert_eq!(seen.len(), 1, "only the list was asked");
            assert!(seen[0].starts_with("GET /v1/computers "), "{}", seen[0]);
            assert!(!seen.iter().any(|r| r.starts_with("POST")), "no start sent");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_reused_pid_that_leads_no_group_is_left_alone() {
        // Started in this test's own group: its pid leads no group.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        terminate_group(child.id(), std::time::Duration::from_millis(200));
        assert!(
            child.try_wait().expect("wait").is_none(),
            "a pid that leads no group gets no signal"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn without_a_terminal_nothing_is_sent_even_with_yes() {
        let (origin, seen) = loopback_cloud(vec![(
            200,
            r#"{"computer":{"id":"cmp_1","name":"dev","state":"starting"}}"#.into(),
        )]);
        let client = lean_ctx_cloud::transport::CloudClient::new(&origin).expect("client");
        let args = parse(&["--cloud", "fix it", "--json", "--yes"]).expect("args");
        let failure = start_paid(
            &lean_ctx_cloud::computers::on_client(&client),
            &listed_row(),
            "req_12345678",
            &args,
            false,
        )
        .expect_err("refused locally");
        assert_eq!(failure.code, Code::ConfirmationRequired);
        assert_eq!(
            failure.message,
            "Starting a paid computer needs you at a terminal or in LeanCTX Code."
        );
        assert_eq!(failure.message, UNATTENDED);
        assert!(seen.lock().expect("seen").is_empty(), "nothing was sent");
    }

    fn paths_in(dir: &Path) -> ssh_types::Paths {
        ssh_types::Paths {
            registry: dir.join("computers.json"),
            ssh_config: dir.join("config"),
            leanctx_config: dir.join("leanctx_config"),
            known_hosts: dir.join("known_hosts"),
        }
    }

    #[test]
    fn a_computer_is_registered_only_with_the_persons_cloud_user() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = paths_in(dir.path());
        let fp = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        for (ssh, why) in [
            (
                json!({ "hostname": "203.0.113.7", "host_key_fingerprints": [fp] }),
                "missing",
            ),
            (
                json!({ "hostname": "203.0.113.7", "user": "leanctx", "host_key_fingerprints": [fp] }),
                "default",
            ),
            (
                json!({ "hostname": "203.0.113.7", "user": "root", "host_key_fingerprints": [fp] }),
                "root",
            ),
            (
                json!({ "hostname": "203.0.113.7", "user": "lc-ABCDEFGH", "host_key_fingerprints": [fp] }),
                "case",
            ),
        ] {
            let mut row = listed_row();
            row.insert("ssh".into(), ssh);
            let failure = ensure_registered(&paths, "dev", &row).expect_err(why);
            assert_eq!(failure.code, Code::Unavailable, "{why}");
            assert!(
                failure.message.contains("SSH user"),
                "{why}: {}",
                failure.message
            );
        }
        assert!(!paths.registry.exists(), "nothing was registered");
    }

    #[test]
    fn text_from_the_computer_is_filtered() {
        let token = format!("ghp_{}", "a1B2".repeat(9));
        let shown = remote_text(&format!("bad\u{1b}[2J thing\r\n {token}"));
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(!shown.contains(&token), "{shown:?}");
        assert!(remote_text(&"x".repeat(5000)).chars().count() <= 300);
        let failure = remote_doc(
            r#"{"kind":"error","error":{"code":"no\u001bpe","message":"line\u0007 one\nsecret"}}"#,
            "run.ack",
        )
        .expect_err("error doc");
        assert!(
            !failure.message.chars().any(char::is_control),
            "{}",
            failure.message
        );
        assert!(
            failure.message.ends_with("(internal)"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn a_stopped_computer_ends_its_run_with_a_reason() {
        let row = |v: Value| v.as_object().expect("row").clone();
        assert_eq!(
            ended_with_computer(&row(
                json!({ "state": "stopped", "stop_reason": "balance_empty" })
            )),
            Some("balance_empty")
        );
        assert_eq!(
            ended_with_computer(&row(json!({ "state": "stopped" }))),
            Some("computer_stopped")
        );
        assert_eq!(
            ended_with_computer(&row(json!({ "state": "running" }))),
            None
        );
        assert_eq!(
            ended_with_computer(&row(json!({ "state": "starting" }))),
            None
        );
    }

    #[test]
    fn fetch_creates_then_fast_forwards_the_run_branch() {
        let (_dir, repo) = repository();
        let first = git_ok(&repo, &["rev-parse", "HEAD"]);
        let remote = "refs/remotes/leanctx-dev/run/run_00000000000000aa";
        let branch = "leanctx/run/run_00000000000000aa";
        git_ok(&repo, &["update-ref", remote, &first]);
        update_local_branch(&repo, branch, remote).expect("created");
        assert_eq!(git_ok(&repo, &["rev-parse", branch]), first);
        // A second fetch with a newer tip moves the branch.
        std::fs::write(repo.join("a.txt"), "two\n").expect("write");
        git_ok(&repo, &["commit", "--quiet", "-am", "second"]);
        let second = git_ok(&repo, &["rev-parse", "HEAD"]);
        git_ok(&repo, &["update-ref", remote, &second]);
        update_local_branch(&repo, branch, remote).expect("fast-forwarded");
        assert_eq!(git_ok(&repo, &["rev-parse", branch]), second);
        // A branch with a commit of its own is left as is.
        git_ok(&repo, &["checkout", "--quiet", branch]);
        std::fs::write(repo.join("a.txt"), "mine\n").expect("write");
        git_ok(&repo, &["commit", "--quiet", "-am", "mine"]);
        let mine = git_ok(&repo, &["rev-parse", "HEAD"]);
        git_ok(&repo, &["checkout", "--quiet", "-"]);
        let failure = update_local_branch(&repo, branch, remote).expect_err("diverged");
        assert_eq!(failure.code, Code::Conflict);
        assert_eq!(git_ok(&repo, &["rev-parse", branch]), mine);
    }
}
