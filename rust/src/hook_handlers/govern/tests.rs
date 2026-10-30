// SPDX-License-Identifier: Apache-2.0

use super::bash::is_viewer_command;
use super::*;
use crate::core::patterns::lossless_collapse;

const ALL_ON: Policy = Policy {
    r1_read: RuleMode::On,
    r1_bash: RuleMode::On,
    r2: RuleMode::On,
    r3: RuleMode::Shadow,
};

fn unique_session(tag: &str) -> String {
    format!(
        "gov-{tag}-{}-{:?}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::thread::current().id()
    )
}

fn bash(session: &str, tool_use: &str, cmd: &str, stdout: &str) -> String {
    serde_json::json!({
        "session_id": session,
        "tool_use_id": tool_use,
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "cwd": "/repo",
        "tool_input": { "command": cmd },
        "tool_response": {
            "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false
        },
    })
    .to_string()
}

fn event(session: &str, name: &str, extra: &serde_json::Value) -> String {
    let mut v = serde_json::json!({ "session_id": session, "hook_event_name": name });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    v.to_string()
}

fn edit(session: &str) -> String {
    event(
        session,
        "PostToolUse",
        &serde_json::json!({ "tool_name": "Edit", "tool_input": { "file_path": "/repo/a.rs" } }),
    )
}

fn listing() -> String {
    (0..200).fold(String::new(), |mut s, i| {
        s.push_str(&format!("src/module_{i}/mod.rs\n"));
        s
    })
}

fn replaced_stdout(d: &Decision) -> Option<String> {
    if d.output.is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&d.output).unwrap();
    let out = &v["hookSpecificOutput"];
    assert_eq!(out["hookEventName"], "PostToolUse");
    let resp = &out["updatedToolOutput"];
    // Shape mirrored: every native field is still there.
    for key in ["stderr", "interrupted", "isImage"] {
        assert!(resp.get(key).is_some(), "mirrored field {key} missing");
    }
    resp["stdout"].as_str().map(str::to_owned)
}

#[test]
fn r1_bash_stubs_only_identical_unchanged_reruns() {
    // (steps between the two identical runs, expect a stub on the second run)
    #[derive(Clone, Copy)]
    enum Between {
        Nothing,
        Edit,
        SideEffectCommand,
        ViewerCommand,
        Compaction,
    }
    let cases = [
        (Between::Nothing, true),
        (Between::Edit, false),
        (Between::SideEffectCommand, false),
        (Between::ViewerCommand, true),
        (Between::Compaction, false),
    ];
    for (i, (between, want_stub)) in cases.into_iter().enumerate() {
        let s = unique_session(&format!("r1-{i}"));
        let first = decide(&bash(&s, "t1", "ls -R src", &listing()), &ALL_ON);
        assert!(first.output.is_empty(), "first run is always native");
        match between {
            Between::Nothing => {}
            Between::Edit => drop(decide(&edit(&s), &ALL_ON)),
            Between::SideEffectCommand => {
                drop(decide(&bash(&s, "tx", "touch src/new.rs", ""), &ALL_ON));
            }
            Between::ViewerCommand => {
                drop(decide(&bash(&s, "tx", "git status", "clean\n"), &ALL_ON));
            }
            Between::Compaction => {
                drop(decide(
                    &event(&s, "PostCompact", &serde_json::json!({})),
                    &ALL_ON,
                ));
            }
        }
        let second = decide(&bash(&s, "t2", "ls -R src", &listing()), &ALL_ON);
        let stub = replaced_stdout(&second);
        assert_eq!(stub.is_some(), want_stub, "case {i}");
        if let Some(stub) = stub {
            assert!(
                stub.contains("[unchanged 200L · lean-ctx governor]"),
                "{stub}"
            );
            let r = second.receipt.as_ref().expect("receipt");
            assert_eq!((r.rule_id, r.decision()), ("R1.bash", "applied"));
            assert!(r.baseline_tokens > r.actual_tokens + MIN_SAVED_TOKENS);
        }
    }
}

#[test]
fn r1_bash_passes_changed_output_duplicates_and_shadow() {
    let s = unique_session("r1b");
    let _ = decide(&bash(&s, "t1", "ls -R src", &listing()), &ALL_ON);
    let changed = format!("{}src/extra.rs\n", listing());
    assert!(
        decide(&bash(&s, "t2", "ls -R src", &changed), &ALL_ON)
            .output
            .is_empty()
    );
    // Duplicate fire of the same tool_use_id is not a re-run.
    assert!(
        decide(&bash(&s, "t2", "ls -R src", &changed), &ALL_ON)
            .output
            .is_empty()
    );

    // Shadow: same decision, receipt only, native output kept.
    let shadow = Policy {
        r1_bash: RuleMode::Shadow,
        ..ALL_ON
    };
    let d = decide(&bash(&s, "t3", "ls -R src", &changed), &shadow);
    assert!(d.output.is_empty(), "shadow never changes the result");
    let r = d.receipt.expect("shadow receipt");
    assert_eq!((r.rule_id, r.decision()), ("R1.bash", "shadow"));
}

#[test]
fn r1_bash_stub_is_deterministic() {
    let render = |tag: &str| {
        let s = unique_session(tag);
        let _ = decide(&bash(&s, "t1", "cat Cargo.toml", &listing()), &ALL_ON);
        replaced_stdout(&decide(
            &bash(&s, "t2", "cat Cargo.toml", &listing()),
            &ALL_ON,
        ))
    };
    let a = render("det-a").expect("stub");
    assert_eq!(Some(a), render("det-b"), "#498: same input, same bytes");
}

#[test]
fn viewer_classifier_table() {
    let cases: &[(&str, bool)] = &[
        ("ls -la", true),
        ("cat src/main.rs", true),
        ("grep -rn foo src | head -20", true),
        ("sed -n '1,20p' a.rs", true),
        ("git status", true),
        ("git diff HEAD~1", true),
        ("find . -name '*.rs'", true),
        ("cargo test 2>&1 | tail -5", false),
        ("sed -i 's/a/b/' a.rs", false),
        ("find . -name x -delete", false),
        ("tail -f log.txt", false),
        ("cat a > b", false),
        ("ls && rm -rf x", false),
        ("echo $(cat a)", false),
        ("git checkout main", false),
        ("touch x", false),
    ];
    for (cmd, want) in cases {
        assert_eq!(is_viewer_command(cmd), *want, "{cmd}");
    }
}

/// Large non-diagnostic output with long identical runs, on a command the
/// shell policy classes as compressible.
fn progress_output() -> String {
    let mut s = String::from("Fetching index\n");
    for chunk in 0..6 {
        for _ in 0..120 {
            s.push_str(&format!(
                "downloading shard {chunk} of the package registry mirror\n"
            ));
        }
    }
    s.push_str("done\n");
    s
}

const BUILD_CMD: &str = "cargo build --release";

#[test]
fn r2_collapse_is_lossless_and_applied_only_when_worthwhile() {
    assert!(
        !crate::shell::output_policy::classify(BUILD_CMD, &[]).is_protected(),
        "fixture command must be compressible"
    );
    let s = unique_session("r2");
    let original = progress_output();
    let d = decide(&bash(&s, "t1", BUILD_CMD, &original), &ALL_ON);
    let delivered = replaced_stdout(&d).expect("R2 applies to large repeated output");
    assert_eq!(
        lossless_collapse::expand(&delivered),
        original,
        "exact inverse"
    );
    let r = d.receipt.expect("receipt");
    assert_eq!(r.rule_id, "R2");
    assert!(r.actual_tokens * 4 <= r.baseline_tokens * 3, "saves ≥ 25 %");

    // Small output: below the trigger, untouched.
    let small = "line\n".repeat(50);
    assert!(
        decide(&bash(&s, "t2", BUILD_CMD, &small), &ALL_ON)
            .output
            .is_empty()
    );
}

#[test]
fn r2_never_touches_errors_diffs_or_test_results() {
    let tails = [
        "error[E0425]: cannot find value `x`\n",
        "diff --git a/src/a.rs b/src/a.rs\n@@ -1 +1 @@\n",
        "test result: ok. 42 passed; 0 failed\n",
        "thread 'main' panicked at src/main.rs:3:5\n",
        "FAIL src/app.test.ts\n",
    ];
    for (i, tail) in tails.iter().enumerate() {
        let s = unique_session(&format!("r2d-{i}"));
        let out = format!("{}{tail}", progress_output());
        let d = decide(&bash(&s, "t1", BUILD_CMD, &out), &ALL_ON);
        assert!(
            d.output.is_empty(),
            "diagnostic case {i} must stay verbatim"
        );
        assert!(d.receipt.is_none(), "no rule fires on diagnostics ({i})");
    }
}

#[test]
fn r3_is_shadow_only_and_counts_repeated_blocks() {
    let s = unique_session("r3");
    let block = (0..40).fold(String::new(), |mut acc, i| {
        acc.push_str(&format!("row {i} of the report\n"));
        acc
    });
    let policy = Policy {
        r1_bash: RuleMode::Off,
        r2: RuleMode::Off,
        r3: RuleMode::On, // capped by Policy::from_env; here it must still not emit
        ..ALL_ON
    };
    // R3 needs a compressible viewer command: side-effecting commands reset
    // the window store (edit epoch), protected ones skip R2/R3 (design §2 G).
    let cmd = ["git log", "ls -R", "grep -rn row ."]
        .into_iter()
        .find(|c| {
            is_viewer_command(c) && !crate::shell::output_policy::classify(c, &[]).is_protected()
        })
        .expect("a compressible viewer command");
    let first = decide(&bash(&s, "t1", cmd, &block), &policy);
    assert!(first.receipt.is_none());
    let again = format!("header\n{block}");
    let d = decide(&bash(&s, "t2", cmd, &again), &policy);
    let r = d.receipt.expect("R3 receipt");
    assert_eq!(r.rule_id, "R3");
    assert!(d.output.is_empty(), "R3 never changes output in v1");
    assert!(r.baseline_tokens > r.actual_tokens);
}

#[test]
fn r1_read_runs_inside_the_governor_with_the_min_saving_guard() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_READ_DEDUP", "on");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    let body = "fn main() { println!(\"hello governor\"); }\n".repeat(80);
    std::fs::write(&file, &body).unwrap();
    let s = unique_session("r1r");
    let read = |tool_use: &str, body: &str| {
        serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_input": { "file_path": file.to_string_lossy() },
            "tool_response": body,
        })
        .to_string()
    };
    assert!(decide(&read("t1", &body), &ALL_ON).output.is_empty());
    let d = decide(&read("t2", &body), &ALL_ON);
    assert!(!d.output.is_empty(), "unchanged re-read is stubbed");
    assert_eq!(d.receipt.as_ref().map(|r| r.rule_id), Some("R1.read"));

    // An edit on disk resets it: the next read is full again.
    let edited = format!("{body}// edited\n");
    std::fs::write(&file, &edited).unwrap();
    assert!(decide(&read("t3", &edited), &ALL_ON).output.is_empty());
    crate::test_env::remove_var("LEAN_CTX_READ_DEDUP");
}

/// Bench finding (dry setup of c8c1b83e6b): with defaults, R1.read was inert
/// because `read_dedup = auto` sniffs host env markers that hook processes do
/// not carry. A governor install alone must make it apply.
#[test]
fn r1_read_applies_after_a_governor_install_without_any_env() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    for var in [
        "LEAN_CTX_READ_DEDUP",
        "CLAUDE_PROJECT_DIR",
        "CLAUDECODE",
        "CODEBUDDY",
        "LEAN_CTX_GOVERNOR",
        "LEAN_CTX_GOVERNOR_R1_READ",
    ] {
        crate::test_env::remove_var(var);
    }
    let home = tempfile::tempdir().unwrap();
    crate::hooks::agents::claude_governor::install(
        home.path(),
        crate::hooks::agents::claude_governor::GovernorArm::NoMcp,
    );
    assert!(crate::hooks::agents::claude_governor::is_installed(
        home.path()
    ));

    let file = home.path().join("lib.rs");
    let body = "fn main() { println!(\"hello governor\"); }\n".repeat(80);
    std::fs::write(&file, &body).unwrap();
    let s = unique_session("r1-default");
    let read = |tool_use: &str| {
        serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_input": { "file_path": file.to_string_lossy() },
            "tool_response": { "type": "text", "file": { "filePath": file.to_string_lossy(),
                "content": body, "numLines": 80 } },
        })
        .to_string()
    };
    let policy = Policy::from_env();
    assert!(
        decide(&read("t1"), &policy).output.is_empty(),
        "first read native"
    );
    let d = decide(&read("t2"), &policy);
    let v: serde_json::Value = serde_json::from_str(&d.output).expect("second read replaced");
    let content = v["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
        .as_str()
        .unwrap();
    assert!(content.contains("[unchanged 80L"), "{content}");
    let r = d.receipt.expect("receipt");
    assert_eq!((r.rule_id, r.decision()), ("R1.read", "applied"));

    // An explicit opt-out still wins.
    crate::test_env::set_var("LEAN_CTX_READ_DEDUP", "off");
    assert!(decide(&read("t3"), &Policy::from_env()).output.is_empty());
    crate::test_env::remove_var("LEAN_CTX_READ_DEDUP");
}

#[test]
fn session_start_purges_only_for_clear_compact_resume() {
    for (source, want_stub) in [
        ("startup", true),
        ("clear", false),
        ("compact", false),
        ("resume", false),
    ] {
        let s = unique_session(source);
        let _ = decide(&bash(&s, "t1", "ls -R src", &listing()), &ALL_ON);
        let start = event(&s, "SessionStart", &serde_json::json!({ "source": source }));
        assert!(
            decide(&start, &ALL_ON).output.is_empty(),
            "SessionStart is silent"
        );
        let d = decide(&bash(&s, "t2", "ls -R src", &listing()), &ALL_ON);
        assert_eq!(!d.output.is_empty(), want_stub, "source {source}");
    }
}

#[test]
fn slow_work_past_the_deadline_yields_native_output() {
    let slow = run_with_deadline(std::time::Duration::from_millis(30), || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        "late".to_string()
    });
    assert_eq!(slow, None, "past the deadline the hook emits nothing");
    let fast = run_with_deadline(DEADLINE, || "ok".to_string());
    assert_eq!(fast.as_deref(), Some("ok"));
}

#[test]
fn concurrent_record_writes_never_tear() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(dir.path().join("rec.gvb"));
    let values: Vec<String> = (0..8)
        .map(|i| format!("{}:{} toolu_{i}", "a".repeat(32 + i), "e".repeat(16)))
        .collect();
    read_dedup::write_atomic(&store, values[0].as_bytes()).unwrap();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (store, done, values) = (store.clone(), done.clone(), values.clone());
        std::thread::spawn(move || {
            let mut reads = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let raw = match std::fs::read_to_string(&*store) {
                    Ok(raw) => raw,
                    // Windows refuses to open a file while a rename replaces it;
                    // the readers in read_dedup treat any read error as no record.
                    Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied => {
                        continue;
                    }
                    Err(e) => panic!("record always exists: {e}"),
                };
                assert!(values.contains(&raw), "torn record: {raw:?}");
                assert!(read_dedup::parse_record(&raw).is_some());
                reads += 1;
            }
            reads
        })
    };
    let writers: Vec<_> = values
        .iter()
        .cloned()
        .map(|v| {
            let store = store.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    read_dedup::write_atomic(&store, v.as_bytes()).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
    let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(leftovers, 1, "no temp files left behind");
}

#[test]
fn policy_env_caps_every_rule() {
    let _lock = crate::core::data_dir::test_env_lock();
    for var in [
        "LEAN_CTX_GOVERNOR",
        "LEAN_CTX_GOVERNOR_R1_READ",
        "LEAN_CTX_GOVERNOR_R1_BASH",
        "LEAN_CTX_GOVERNOR_R2",
        "LEAN_CTX_GOVERNOR_R3",
    ] {
        crate::test_env::remove_var(var);
    }
    let defaults = Policy::from_env();
    assert_eq!(
        defaults,
        Policy {
            r1_read: RuleMode::On,
            r1_bash: RuleMode::Shadow,
            r2: RuleMode::Shadow,
            r3: RuleMode::Shadow,
        },
        "v1 defaults: R1.read live, everything new log-only"
    );
    crate::test_env::set_var("LEAN_CTX_GOVERNOR_R2", "on");
    crate::test_env::set_var("LEAN_CTX_GOVERNOR_R3", "on");
    let p = Policy::from_env();
    assert_eq!((p.r2, p.r3), (RuleMode::On, RuleMode::Shadow), "R3 capped");
    crate::test_env::set_var("LEAN_CTX_GOVERNOR", "shadow");
    assert_eq!(Policy::from_env().r1_read, RuleMode::Shadow, "global cap");
    crate::test_env::set_var("LEAN_CTX_GOVERNOR", "off");
    assert_eq!(Policy::from_env().r2, RuleMode::Off);
    for var in [
        "LEAN_CTX_GOVERNOR",
        "LEAN_CTX_GOVERNOR_R2",
        "LEAN_CTX_GOVERNOR_R3",
    ] {
        crate::test_env::remove_var(var);
    }
}

#[test]
fn unknown_shapes_and_tools_pass_through() {
    let s = unique_session("shape");
    let cases = [
        "not json".to_string(),
        serde_json::json!({ "session_id": s, "hook_event_name": "PostToolUse",
            "tool_name": "Bash", "tool_input": { "command": "ls" },
            "tool_response": "plain string" })
        .to_string(),
        serde_json::json!({ "session_id": s, "hook_event_name": "PostToolUse",
            "tool_name": "Grep", "tool_response": { "content": listing() } })
        .to_string(),
        // No session id: never stub.
        serde_json::json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": { "command": "ls" }, "tool_response": { "stdout": listing() } })
        .to_string(),
    ];
    for c in cases {
        let d = decide(&c, &ALL_ON);
        assert!(d.output.is_empty() && d.receipt.is_none(), "{c}");
    }
}

#[test]
fn enforce_hook_keeps_the_secret_path_deny() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
    let read = |path: &str| {
        serde_json::json!({ "tool_name": "Read", "hook_event_name": "PreToolUse",
            "transcript_path": "/t", "tool_input": { "file_path": path } })
        .to_string()
    };
    let decision = |path: &str| -> serde_json::Value {
        let out = enforce::enforce_decision(&read(path), "lean-ctx");
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["hookSpecificOutput"]
            ["permissionDecision"]
            .clone()
    };
    assert_eq!(decision("/repo/.env"), "deny", "secret-like path denied");
    assert!(
        decision("/repo/src/main.rs").is_null(),
        "ordinary read untouched"
    );
}

#[test]
fn enforce_hook_never_grants_a_permission() {
    let binary = "lean-ctx";
    let bash = |cmd: &str| {
        serde_json::json!({ "tool_name": "Bash", "hook_event_name": "PreToolUse",
            "transcript_path": "/t", "tool_input": { "command": cmd } })
        .to_string()
    };
    let risky = "true && python3 -c 'print(1)'";
    let inputs = [
        String::new(),
        "not json".to_owned(),
        serde_json::json!({ "tool_name": "Read", "tool_input": { "file_path": "/repo/a.rs" } })
            .to_string(),
        serde_json::json!({ "tool_name": "PowerShell", "tool_input": { "command": "ls" } })
            .to_string(),
        bash("cat src/main.rs"),
        bash(risky),
    ];
    for input in &inputs {
        let out = enforce::enforce_decision(input, binary);
        assert!(!out.contains("allow"), "{input} -> {out}");
    }
    assert_eq!(
        enforce::enforce_decision(&bash("cat src/main.rs"), binary),
        "{}"
    );
}

#[test]
fn enforce_only_rewrite_never_compresses() {
    let binary = "lean-ctx";
    let payload = |cmd: &str| {
        serde_json::json!({ "tool_name": "Bash", "hook_event_name": "PreToolUse",
            "transcript_path": "/t", "tool_input": { "command": cmd } })
        .to_string()
    };
    // A plain read command that `hook rewrite` would route through ctx_read /
    // compression stays native here.
    let out: serde_json::Value = serde_json::from_str(&enforce::enforce_decision(
        &payload("cat src/main.rs"),
        binary,
    ))
    .unwrap();
    assert!(out.get("hookSpecificOutput").is_none(), "{out}");

    // When the allowlist demands a wrap and the host's rules let it through,
    // it is the raw (uncompressed) wrap.
    let risky = "true && python3 -c 'print(1)'";
    if crate::hook_handlers::file_rewrite::needs_enforcement_wrap(risky, binary) {
        let out: serde_json::Value =
            serde_json::from_str(&enforce::enforce_decision(&payload(risky), binary)).unwrap();
        if let Some(cmd) = out["hookSpecificOutput"]["updatedInput"]["command"].as_str() {
            assert!(cmd.starts_with("lean-ctx -c --raw "), "{cmd}");
        }
    }
}
