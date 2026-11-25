// SPDX-License-Identifier: Apache-2.0

use super::bash::is_viewer_command;
use super::*;
use lean_ctx::core::patterns::lossless_collapse;

const ALL_ON: Policy = Policy {
    r1_read: RuleMode::On,
    r1_bash: RuleMode::On,
    r2: RuleMode::On,
    r3: RuleMode::Shadow,
    // R4 needs an archive and a project; its tests turn it on explicitly.
    r4: RuleMode::Off,
    r5: RuleMode::Off,
    r6: RuleMode::On,
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

/// A 540-line Rust file whose every line names `tag`, so two fixtures never
/// share content (views match by content).
fn code_file(tag: &str) -> String {
    (0..60).fold(String::new(), |mut acc, i| {
        acc.push_str(&format!(
            "pub fn {tag}_{i}(input: &str) -> usize {{\n    let trimmed = input.trim(); // {tag}\n    \
             let words = trimmed.split_whitespace().count();\n    let bytes = trimmed.len() + {i};\n    \
             if words > bytes {{ // {tag}\n        return words;\n    }}\n    bytes * 2 + words\n}}\n"
        ));
        acc
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
            // gain/cockpit count the applied saving under the rule.
            assert_eq!(
                second.applied_stats(),
                Some((
                    "governor:R1.bash".to_string(),
                    r.baseline_tokens,
                    r.actual_tokens
                ))
            );
            // The same rule in shadow delivers the native result: no saving.
            let mut shadow = ALL_ON;
            shadow.r1_bash = RuleMode::Shadow;
            let third = decide(&bash(&s, "t3", "ls -R src", &listing()), &shadow);
            assert!(third.output.is_empty());
            assert_eq!(third.applied_stats(), None);
        } else {
            assert_eq!(second.applied_stats(), None, "case {i}");
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
fn hook_calls_count_host_tools_but_not_leanctx_mcp_tools() {
    let s = unique_session("hookcount");
    let host = decide(&bash(&s, "t1", "git status", "clean\n"), &ALL_ON);
    assert_eq!(host.hook_session.as_deref(), Some(s.as_str()));
    let ctx_read = serde_json::json!({
        "session_id": s,
        "tool_use_id": "t2",
        "hook_event_name": "PostToolUse",
        "tool_name": "mcp__lean-ctx__ctx_read",
        "cwd": "/repo",
        "tool_input": { "path": "src/lib.rs" },
        "tool_response": "fn main() {}",
    })
    .to_string();
    assert_eq!(
        decide(&ctx_read, &ALL_ON).hook_session,
        None,
        "the MCP server already counts its own ctx_read; the hook must not count it again"
    );
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
        !lean_ctx::shell::output_policy::classify(BUILD_CMD, &[]).is_protected(),
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

const R4_ONLY: Policy = Policy {
    r1_read: RuleMode::Off,
    r1_bash: RuleMode::Off,
    r2: RuleMode::Off,
    r3: RuleMode::Off,
    r4: RuleMode::On,
    r5: RuleMode::Off,
    r6: RuleMode::Off,
};
/// A compressible command (asserted in the R2 test). File viewers such as
/// `cat` are verbatim-class: no governor rule touches their output.
const LOG_CMD: &str = BUILD_CMD;
const RARE_ERROR: &str = "2026-03-14T03:12:09.114Z ERROR worker-2 batch b-006992 stopped: \
                          ValueError: invalid duration '5x' raised in parse_duration";

/// A day's log: heartbeats and transient retries differing only in
/// timestamps, ids and counters, with one rare error in the middle.
fn timestamped_log() -> String {
    let mut s = String::new();
    for i in 0..1500usize {
        let (m, sec, ms) = ((i / 60) % 60, i % 60, (i * 37) % 1000);
        s.push_str(&format!(
            "2026-03-14T03:{m:02}:{sec:02}.{ms:03}Z INFO heartbeat ok worker-1 seq={i}\n"
        ));
        if i % 3 == 0 {
            s.push_str(&format!(
                "2026-03-14T03:{m:02}:{sec:02}.{ms:03}Z WARN worker-1 delivery dlv-{:08x} \
                 failed: 503 Service Unavailable (attempt {}/5, retrying in 500ms)\n",
                0x1000_0000 + i * 7919,
                i % 5 + 1
            ));
        }
        if i == 977 {
            s.push_str(RARE_ERROR);
            s.push('\n');
        }
    }
    s
}

fn bash_in(session: &str, cwd: &std::path::Path, cmd: &str, stdout: &str) -> String {
    let mut v: serde_json::Value = serde_json::from_str(&bash(session, "t1", cmd, stdout)).unwrap();
    v["cwd"] = serde_json::Value::String(cwd.to_string_lossy().into_owned());
    v.to_string()
}

/// A git project, an isolated archive and community policy for R4 tests.
fn r4_project() -> (
    lean_ctx::core::data_dir::IsolatedDataDir,
    lean_ctx::core::policy::runtime::TestPolicyOverride,
    tempfile::TempDir,
) {
    let data = lean_ctx::core::data_dir::isolated_data_dir();
    let community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".git")).unwrap();
    (data, community, project)
}

#[test]
fn r4_groups_a_timestamped_log_keeps_the_rare_error_and_archives_the_exact_original() {
    let (_data, _policy, project) = r4_project();
    assert!(!lean_ctx::shell::output_policy::classify(LOG_CMD, &[]).is_protected());
    let original = timestamped_log();
    let payload = bash_in(&unique_session("r4"), project.path(), LOG_CMD, &original);
    let d = decide(&payload, &R4_ONLY);
    let shown = replaced_stdout(&d).expect("R4 applies");
    let r = d.receipt.expect("receipt");
    assert_eq!(r.rule_id, "R4");
    assert!(
        r.actual_tokens * 5 <= r.baseline_tokens,
        "saves ≥ 80 %: {r:?}"
    );
    assert!(
        shown.lines().any(|l| l == RARE_ERROR),
        "rare error verbatim"
    );
    assert!(shown.contains("[1500× like] "), "{shown}");
    // Deterministic (#498): the same output gives the same bytes.
    let again = decide(
        &bash_in(&unique_session("r4b"), project.path(), LOG_CMD, &original),
        &R4_ONLY,
    );
    assert_eq!(replaced_stdout(&again).as_deref(), Some(shown.as_str()));

    let id = shown
        .split("ctx_expand(id=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("archive id named");
    let root = project.path().to_str().unwrap();
    let (exact, expanded) = lean_ctx::core::policy::runtime::with_project_source_view(root, || {
        // What `ctx_expand(id=…)` resolves for the id: the copy as released
        // now (a recovery id through its checked resolver), re-admitted under
        // the current policy. Its selectors are tested with the tool.
        let released = match lean_ctx::core::archive::RecoveryId::parse(id) {
            Some(recovery_id) => {
                lean_ctx::core::archive::retrieve_recovery_checked(&recovery_id).ok()
            }
            None => lean_ctx::core::archive::retrieve(id),
        };
        (
            lean_ctx::core::archive::retrieve(id),
            released.and_then(|content| {
                lean_ctx::core::context_admission::recovery::admit_recovered(&content, "archive")
                    .ok()
            }),
        )
    })
    .unwrap();
    assert_eq!(exact.as_deref(), Some(original.as_str()), "exact bytes");
    let expanded = expanded.expect("the archive is released to ctx_expand");
    assert!(expanded.contains(RARE_ERROR) && expanded.contains("seq=1499"));
}

#[test]
fn r4_modes_archive_gate_and_untouched_shapes() {
    let (_data, _policy, project) = r4_project();
    let s = unique_session("r4m");
    let log = timestamped_log();
    let run = |stdout: &str, policy: &Policy| {
        decide(&bash_in(&s, project.path(), LOG_CMD, stdout), policy)
    };

    let shadow = run(
        &log,
        &Policy {
            r4: RuleMode::Shadow,
            ..R4_ONLY
        },
    );
    assert!(shadow.output.is_empty(), "shadow never replaces");
    assert_eq!(shadow.receipt.map(|r| r.rule_id), Some("R4"));
    let off = run(
        &log,
        &Policy {
            r4: RuleMode::Off,
            ..R4_ONLY
        },
    );
    assert!(off.output.is_empty() && off.receipt.is_none());
    // File viewers qualify unless a file read looks like code or data;
    // other verbatim-class commands never do (owner decision 2026-10-31).
    let commands = [
        ("cat logs/relayd.log", true),
        (
            "cd logs && grep -v heartbeat relayd.log | head -80; tail -30 relayd.log",
            true,
        ),
        ("cat relayd/worker.py", false),
        ("head -5000 data/exports/deliveries.jsonl", false),
        (
            "grep -n retry relayd/sender.py config/relayd.ini | head -40",
            false,
        ),
        ("curl -s https://ops.example/logs/relayd", false),
    ];
    for (cmd, fires) in commands {
        let d = decide(&bash_in(&s, project.path(), cmd, &log), &R4_ONLY);
        assert_eq!(!d.output.is_empty(), fires, "{cmd}");
    }

    // Diffs are refused whole; small output is below the trigger.
    let diff = format!("{log}diff --git a/x b/x\n@@ -1,2 +1,2 @@\n");
    for stdout in [
        diff.as_str(),
        "2026-03-14T03:00:00.000Z INFO heartbeat ok seq=1\n",
    ] {
        let d = run(stdout, &R4_ONLY);
        assert!(d.output.is_empty() && d.receipt.is_none());
    }

    // R4 is Bash only: a Read of the same text never gets an R4 receipt.
    let read = serde_json::json!({
        "session_id": s, "hook_event_name": "PostToolUse", "tool_name": "Read",
        "cwd": project.path(), "tool_input": { "file_path": project.path().join("x.log") },
        "tool_response": { "type": "text", "file": { "content": log } },
    });
    let d = decide(
        &read.to_string(),
        &Policy {
            r1_read: RuleMode::On,
            ..R4_ONLY
        },
    );
    assert!(d.receipt.is_none_or(|r| r.rule_id != "R4"));

    // Without a usable archive R4 never fires, in either mode.
    for (var, value) in [("LEAN_CTX_ARCHIVE", "off"), ("LEAN_CTX_MINIMAL", "1")] {
        lean_ctx::test_env::set_var(var, value);
        for mode in [RuleMode::On, RuleMode::Shadow] {
            let d = run(
                &log,
                &Policy {
                    r4: mode,
                    ..R4_ONLY
                },
            );
            assert!(d.output.is_empty() && d.receipt.is_none(), "{var}");
        }
        lean_ctx::test_env::remove_var(var);
    }
}

/// Offline evidence over bench transcripts: `LEAN_CTX_R4_CORPUS` holds
/// `<name>.txt` Bash results with their command in `<name>.cmd`. Prints
/// whether R4 would fire (command guard and thresholds included) and writes
/// the view to `<name>.r4`.
#[test]
#[ignore = "offline evidence; needs LEAN_CTX_R4_CORPUS"]
fn r4_offline_corpus_report() {
    use lean_ctx::core::patterns::template_collapse;
    use lean_ctx::core::tokens::count_tokens;
    let Ok(dir) = std::env::var("LEAN_CTX_R4_CORPUS") else {
        return;
    };
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    paths.sort();
    for path in paths {
        let input = std::fs::read_to_string(&path).unwrap();
        let cmd = std::fs::read_to_string(path.with_extension("cmd")).unwrap_or_default();
        let class = lean_ctx::shell::output_policy::classify(cmd.trim(), &[]);
        let allowed = super::bash::r4_may_collapse(cmd.trim(), class);
        let baseline = count_tokens(&input);
        let view = template_collapse::collapse(&input)
            .map(|c| template_collapse::render(&c, "full: ctx_expand(id=\"ffffffffffffffff\")"));
        let actual = view.as_deref().map_or(baseline, count_tokens);
        let fires = allowed && super::bash::r4_worthwhile(baseline, actual);
        let text = view.unwrap_or_else(|| input.clone());
        std::fs::write(path.with_extension("r4"), &text).unwrap();
        println!(
            "R4 {} class={class:?} allowed={allowed} fires={fires} tokens={baseline}->{actual} chars={}->{}",
            path.file_name().unwrap().to_string_lossy(),
            input.len(),
            text.len(),
        );
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
            is_viewer_command(c) && !lean_ctx::shell::output_policy::classify(c, &[]).is_protected()
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
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_READ_DEDUP", "on");
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
    lean_ctx::test_env::remove_var("LEAN_CTX_READ_DEDUP");
}

/// R5 (owner decision 2026-11-18): the first whole-file Read of a large code
/// file comes as its outline; ranged reads, small files and the second
/// whole-file Read get the native content, and that second Read is not
/// mistaken for an R1 re-read of content the model never saw.
#[test]
fn r5_outlines_the_first_whole_read_of_a_large_code_file_once() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_READ_DEDUP", "on");
    let dir = tempfile::tempdir().unwrap();
    // 60 functions of 9 lines: bodies dominate, as in real code.
    let big = (0..60).fold(String::new(), |mut s, i| {
        s.push_str(&format!(
            "pub fn handler_{i}(input: &str) -> usize {{\n    let trimmed = input.trim_start_matches('#');\n    \
             let words = trimmed.split_whitespace().count();\n    let bytes = trimmed.len() + {i};\n    \
             if words > bytes {{\n        return words;\n    }}\n    bytes * 2 + words\n}}\n"
        ));
        s
    });
    let big_rs = dir.path().join("big.rs");
    std::fs::write(&big_rs, &big).unwrap();
    let small = big.lines().take(40).collect::<Vec<_>>().join("\n");
    let small_rs = dir.path().join("small.rs");
    std::fs::write(&small_rs, &small).unwrap();
    let s = unique_session("r5");
    let read = |tool_use: &str, file: &std::path::Path, body: &str, extra: serde_json::Value| {
        let mut input = serde_json::json!({ "file_path": file.to_string_lossy() });
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_input": input,
            "tool_response": { "type": "text", "file": { "filePath": file.to_string_lossy(),
                "content": body, "numLines": body.lines().count() } },
        })
        .to_string()
    };
    let policy = Policy {
        r5: RuleMode::On,
        r6: RuleMode::Off,
        ..ALL_ON
    };
    let content = |d: &Decision| -> String {
        let v: serde_json::Value = serde_json::from_str(&d.output).expect("replaced");
        v["hookSpecificOutput"]["updatedToolOutput"]["file"]["content"]
            .as_str()
            .unwrap()
            .to_owned()
    };

    let first = decide(&read("t1", &big_rs, &big, serde_json::json!({})), &policy);
    assert_eq!(first.receipt.as_ref().map(|r| r.rule_id), Some("R5"));
    let outline = content(&first);
    assert!(outline.contains("[outline 540L"), "{outline}");
    assert!(
        outline.contains("fn pub handler_7(input:&str) → usize @L64-72"),
        "{outline}"
    );
    assert!(outline.len() * 2 < big.len());
    // A duplicate fire of the same event gets the same answer.
    assert_eq!(
        content(&decide(
            &read("t1", &big_rs, &big, serde_json::json!({})),
            &policy
        )),
        outline
    );

    // Ranged reads and small files pass through.
    let ranged = decide(
        &read(
            "t2",
            &big_rs,
            &big,
            serde_json::json!({ "offset": 64, "limit": 9 }),
        ),
        &policy,
    );
    assert!(ranged.output.is_empty() && ranged.receipt.is_none());
    let small_read = decide(
        &read("t3", &small_rs, &small, serde_json::json!({})),
        &policy,
    );
    assert!(small_read.output.is_empty());

    // The second whole-file Read delivers the full file, not an outline and
    // not an `[unchanged]` stub; only a third one is a real R1 re-read.
    let second = decide(&read("t4", &big_rs, &big, serde_json::json!({})), &policy);
    assert!(second.output.is_empty(), "full content the second time");
    let third = decide(&read("t5", &big_rs, &big, serde_json::json!({})), &policy);
    assert_eq!(third.receipt.as_ref().map(|r| r.rule_id), Some("R1.read"));

    // Shadow: native content and an R5 receipt.
    let s2 = unique_session("r5-shadow");
    let shadow_policy = Policy {
        r5: RuleMode::Shadow,
        r6: RuleMode::Off,
        ..ALL_ON
    };
    let shadow_read = serde_json::json!({
        "session_id": s2, "tool_use_id": "u1", "hook_event_name": "PostToolUse",
        "tool_name": "Read", "tool_input": { "file_path": big_rs.to_string_lossy() },
        "tool_response": { "type": "text", "file": { "content": big } },
    })
    .to_string();
    let d = decide(&shadow_read, &shadow_policy);
    assert!(d.output.is_empty());
    assert_eq!(d.receipt.map(|r| r.rule_id), Some("R5"));
    // The live rule would deliver the second whole Read in full: shadow must
    // not count it as an R5 saving (R1, on here, may stub it).
    let again = decide(&shadow_read.replace("\"u1\"", "\"u2\""), &shadow_policy);
    assert_ne!(again.receipt.map(|r| r.rule_id), Some("R5"));

    // Off, a subagent's own first Read, an upper-case extension, and a file
    // without extractable symbols.
    let s4 = unique_session("r5-more");
    let read_as = |tool_use: &str, agent: &str, file: &std::path::Path, body: &str| {
        serde_json::json!({
            "session_id": s4, "tool_use_id": tool_use, "agent_id": agent,
            "hook_event_name": "PostToolUse", "tool_name": "Read",
            "tool_input": { "file_path": file.to_string_lossy() },
            "tool_response": { "type": "text", "file": { "content": body } },
        })
        .to_string()
    };
    let off = Policy {
        r5: RuleMode::Off,
        r6: RuleMode::Off,
        ..ALL_ON
    };
    let d = decide(&read_as("o1", "", &big_rs, &big), &off);
    assert!(d.output.is_empty() && d.receipt.is_none());
    let main_ctx = decide(&read_as("o2", "", &big_rs, &big), &policy);
    assert_eq!(main_ctx.receipt.map(|r| r.rule_id), Some("R5"));
    let sub_ctx = decide(&read_as("o3", "agent-7", &big_rs, &big), &policy);
    assert_eq!(sub_ctx.receipt.map(|r| r.rule_id), Some("R5"));
    // A name of its own: on a case-insensitive disk "BIG.RS" is big.rs.
    let upper = dir.path().join("UPPER.RS");
    std::fs::write(&upper, &big).unwrap();
    let d = decide(&read_as("o4", "", &upper, &big), &policy);
    assert_eq!(d.receipt.map(|r| r.rule_id), Some("R5"));
    let plain = "just a sentence without any code in it\n".repeat(400);
    let no_symbols = dir.path().join("notes.rs");
    std::fs::write(&no_symbols, &plain).unwrap();
    let d = decide(&read_as("o5", "", &no_symbols, &plain), &policy);
    assert!(d.output.is_empty());

    // A Read cut at the host's line cap: the outline names the uncovered
    // lines instead of claiming the whole file.
    let s3 = unique_session("r5-capped");
    let capped_read = serde_json::json!({
        "session_id": s3, "tool_use_id": "c1", "hook_event_name": "PostToolUse",
        "tool_name": "Read", "tool_input": { "file_path": big_rs.to_string_lossy() },
        "tool_response": { "type": "text", "file": { "content": big, "totalLines": 5000 } },
    })
    .to_string();
    let capped = content(&decide(&capped_read, &policy));
    assert!(capped.contains("[outline 1-540 of 5000L"), "{capped}");
    assert!(
        capped.contains("Lines 541-5000 are not covered"),
        "{capped}"
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_READ_DEDUP");
}

/// Bench finding (dry setup of c8c1b83e6b): with defaults, R1.read was inert
/// because `read_dedup = auto` sniffs host env markers that hook processes do
/// not carry. A governor install alone must make it apply.
#[test]
fn r1_read_applies_after_a_governor_install_without_any_env() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    for var in [
        "LEAN_CTX_READ_DEDUP",
        "CLAUDE_PROJECT_DIR",
        "CLAUDECODE",
        "CODEBUDDY",
        "LEAN_CTX_GOVERNOR",
        "LEAN_CTX_GOVERNOR_R1_READ",
    ] {
        lean_ctx::test_env::remove_var(var);
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
    // The install persists the measured rule modes: no env needed.
    let policy = Policy::load();
    assert_eq!(
        (policy.r1_bash, policy.r2, policy.r3, policy.r4),
        (RuleMode::On, RuleMode::On, RuleMode::Shadow, RuleMode::On),
        "install wrote [governor] rule modes"
    );
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
    lean_ctx::test_env::set_var("LEAN_CTX_READ_DEDUP", "off");
    assert!(decide(&read("t3"), &Policy::load()).output.is_empty());
    lean_ctx::test_env::remove_var("LEAN_CTX_READ_DEDUP");
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
                    // Windows refuses to open a file while a rename replaces it
                    // (PermissionDenied) and can briefly report it missing
                    // (NotFound); the readers in read_dedup treat any read
                    // error as no record.
                    Err(e)
                        if cfg!(windows)
                            && matches!(
                                e.kind(),
                                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
                            ) =>
                    {
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
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for var in [
        "LEAN_CTX_GOVERNOR",
        "LEAN_CTX_GOVERNOR_R1_READ",
        "LEAN_CTX_GOVERNOR_R1_BASH",
        "LEAN_CTX_GOVERNOR_R2",
        "LEAN_CTX_GOVERNOR_R3",
        "LEAN_CTX_GOVERNOR_R4",
        "LEAN_CTX_GOVERNOR_R5",
    ] {
        lean_ctx::test_env::remove_var(var);
    }
    use lean_ctx::core::config::governor::GovernorConfig;
    let unset = GovernorConfig::default();
    let defaults = Policy::resolve(&unset);
    assert_eq!(
        defaults,
        Policy {
            r1_read: RuleMode::On,
            r1_bash: RuleMode::Shadow,
            r2: RuleMode::Shadow,
            r3: RuleMode::Shadow,
            r4: RuleMode::Shadow,
            r5: RuleMode::On,
            r6: RuleMode::Shadow,
        },
        "built-in defaults without config: R1.read and R5 live, the rest log-only"
    );
    // `[governor]` keys apply without env; R3 stays capped at shadow.
    let mut installed = GovernorConfig {
        r3: Some("on".into()),
        ..Default::default()
    };
    installed.fill_install_defaults();
    assert_eq!(
        Policy::resolve(&installed),
        Policy {
            r1_read: RuleMode::On,
            r1_bash: RuleMode::On,
            r2: RuleMode::On,
            r3: RuleMode::Shadow,
            r4: RuleMode::On,
            r5: RuleMode::On,
            r6: RuleMode::On,
        },
        "config rule modes honoured"
    );
    // Env beats config.
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR_R2", "off");
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR_R1_BASH", "shadow");
    let p = Policy::resolve(&installed);
    assert_eq!(
        (p.r1_bash, p.r2),
        (RuleMode::Shadow, RuleMode::Off),
        "env wins"
    );
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR_R2", "on");
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR_R3", "on");
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR_R4", "on");
    let p = Policy::resolve(&unset);
    assert_eq!(
        (p.r2, p.r3, p.r4),
        (RuleMode::On, RuleMode::Shadow, RuleMode::On),
        "R3 capped"
    );
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR", "shadow");
    assert_eq!(
        Policy::resolve(&installed).r1_read,
        RuleMode::Shadow,
        "global cap"
    );
    lean_ctx::test_env::set_var("LEAN_CTX_GOVERNOR", "off");
    assert_eq!(Policy::resolve(&installed).r2, RuleMode::Off);
    for var in [
        "LEAN_CTX_GOVERNOR",
        "LEAN_CTX_GOVERNOR_R1_BASH",
        "LEAN_CTX_GOVERNOR_R2",
        "LEAN_CTX_GOVERNOR_R3",
        "LEAN_CTX_GOVERNOR_R4",
    ] {
        lean_ctx::test_env::remove_var(var);
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
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::remove_var("LEAN_CTX_IO_BOUNDARY_MODE");
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
    // The verdicts read the allowlist from process-wide env, which other
    // tests set under this lock (an override without `cat`).
    let _lock = lean_ctx::core::data_dir::test_env_lock();
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
    // See `enforce_hook_never_grants_a_permission`.
    let _lock = lean_ctx::core::data_dir::test_env_lock();
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

/// The governor's hook let PowerShell through with no check at all; the
/// `lean-ctx -c` wrap cannot carry it, so it is checked and denied instead.
#[test]
fn enforce_hook_denies_powershell_that_fails_the_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "git,cargo");
    let out = enforce::enforce_decision(
        &serde_json::json!({ "tool_name": "PowerShell", "hook_event_name": "PreToolUse",
            "transcript_path": "/t", "tool_input": { "command": "python -c 'x'" } })
        .to_string(),
        "lean-ctx",
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "{out}"
    );
}

/// GitHub #2147 (governor profile): PreCompact/PostCompact reach `hook govern`,
/// not `hook observe`. The MCP server only learns of a compaction from the
/// `last_compaction.json` marker, so govern must write it too — otherwise
/// `ctx_read` keeps answering "unchanged" for content the model lost.
#[test]
fn governed_compaction_events_write_the_compaction_marker() {
    let iso = lean_ctx::core::data_dir::isolated_data_dir();
    let marker = iso.path().join("last_compaction.json");
    let s = unique_session("compact-marker");
    for (name, extra) in [
        ("PreCompact", serde_json::json!({ "trigger": "auto" })),
        ("PostCompact", serde_json::json!({ "trigger": "manual" })),
        ("SessionStart", serde_json::json!({ "source": "compact" })),
        ("SessionStart", serde_json::json!({ "source": "clear" })),
    ] {
        let _ = std::fs::remove_file(&marker);
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let d = decide(&event(&s, name, &extra), &ALL_ON);
        assert!(d.output.is_empty(), "{name}: lifecycle events stay silent");
        let raw = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{name} {extra}: no compaction marker ({e})"));
        let ts = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["ts"]
            .as_u64()
            .expect("marker ts");
        assert!(ts >= before, "{name}: marker ts {ts} older than {before}");
    }

    // A fresh start, a resume (which purges), a start without a source or an
    // ordinary tool call is no compaction.
    for extra in [
        serde_json::json!({ "source": "startup" }),
        serde_json::json!({ "source": "resume" }),
        serde_json::json!({}),
    ] {
        let _ = std::fs::remove_file(&marker);
        drop(decide(&event(&s, "SessionStart", &extra), &ALL_ON));
        assert!(
            !marker.exists(),
            "SessionStart {extra}: no marker without a compaction"
        );
    }
    let _ = std::fs::remove_file(&marker);
    drop(decide(&edit(&s), &ALL_ON));
    assert!(!marker.exists(), "Edit: no marker without a compaction");
}

/// Code Mode: a REPL script always gets native tool results; the governor
/// view replaces them only where the script hands them to the model verbatim.
#[test]
fn code_mode_never_rewrites_inner_calls_and_shapes_the_repl_result() {
    let s = unique_session("repl");
    let policy = Policy {
        r5: RuleMode::On,
        r6: RuleMode::Off,
        ..ALL_ON
    };
    let repl = |tool_use: &str, result: &str, stdout: &str| {
        event(
            &s,
            "PostToolUse",
            &serde_json::json!({ "tool_use_id": tool_use, "tool_name": "REPL",
                "tool_input": { "code": "…" },
                "tool_response": { "code": "…", "result": result, "stdout": stdout,
                    "stderr": "", "toolsUsed": ["Bash"] } }),
        )
    };
    let shaped = |d: &Decision| -> serde_json::Value {
        assert_eq!(d.receipt.as_ref().map(|r| r.rule_id), Some("REPL"));
        let v: serde_json::Value = serde_json::from_str(&d.output).unwrap();
        v["hookSpecificOutput"]["updatedToolOutput"].clone()
    };

    // R1: an inner viewer leaves no record, so the model's own `ls` with the
    // same output is not stubbed as already seen.
    let ls = listing();
    assert!(
        decide(&bash(&s, "repl_a", "ls src", &ls), &policy)
            .output
            .is_empty()
    );
    assert!(
        decide(&bash(&s, "m1", "ls src", &ls), &policy)
            .output
            .is_empty()
    );

    // R2: the script gets the native output …
    let original = progress_output();
    assert!(
        decide(&bash(&s, "repl_b", BUILD_CMD, &original), &policy)
            .output
            .is_empty()
    );
    // … a cut of it reaches the model unchanged, and the finished eval
    // consumes the view …
    assert!(
        decide(&repl("r1", &format!("'{}'", &original[..40]), ""), &policy)
            .output
            .is_empty()
    );
    assert!(
        decide(&repl("r1b", "undefined", &original), &policy)
            .output
            .is_empty(),
        "a view never outlives its eval"
    );
    // … and in the eval of a new inner call, verbatim it arrives collapsed: in console output and inside the
    // `'…\n' +` literal chain util.inspect prints for a returned string.
    let inspect = original
        .split_inclusive('\n')
        .map(|l| format!("'{}'", l.replace('\n', "\\n")))
        .collect::<Vec<_>>()
        .join(" +\n  ");
    assert!(
        decide(&bash(&s, "repl_b2", BUILD_CMD, &original), &policy)
            .output
            .is_empty()
    );
    let out = shaped(&decide(
        &repl("r2", &format!("{{ log: {inspect} }}"), &original),
        &policy,
    ));
    let collapsed = out["stdout"].as_str().unwrap();
    assert_eq!(lossless_collapse::expand(collapsed), original);
    let result = out["result"].as_str().unwrap();
    let chain = super::repl::parse_chain(result, result.find('\'').unwrap())
        .expect("re-encoded as a literal chain");
    assert_eq!(lossless_collapse::expand(&chain.decoded), original);
    assert!(
        result.starts_with("{ log: '") && result.ends_with(" }"),
        "{result}"
    );

    // R5: an inner whole-file Read keeps its content for the script; the
    // numbered text the script got becomes the outline for the model.
    let dir = tempfile::tempdir().unwrap();
    let inner_read = |tool_use: &str, path: &std::path::Path, content: &str| {
        serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "Read", "tool_input": { "file_path": path.to_string_lossy() },
            "tool_response": { "type": "text", "file": { "filePath": path.to_string_lossy(),
                "content": content, "numLines": content.lines().count() } },
        })
        .to_string()
    };
    let numbered = |content: &str| {
        content
            .lines()
            .enumerate()
            .fold(String::new(), |mut acc, (i, l)| {
                use std::fmt::Write as _;
                let _ = writeln!(acc, "{:>6}\t{l}", i + 1);
                acc
            })
    };
    let big = code_file("handler");
    let path = dir.path().join("big.rs");
    std::fs::write(&path, &big).unwrap();
    assert!(
        decide(&inner_read("repl_c", &path, &big), &policy)
            .output
            .is_empty()
    );
    let out = shaped(&decide(&repl("r3", "undefined", &numbered(&big)), &policy));
    let stdout = out["stdout"].as_str().unwrap();
    assert!(stdout.contains("[outline 540L"), "{stdout}");
    assert!(stdout.contains("handler_7"), "{stdout}");
    // A second inner whole Read is R5's escape: its eval hands back the full
    // text.
    assert!(
        decide(&inner_read("repl_c2", &path, &big), &policy)
            .output
            .is_empty()
    );
    assert!(
        decide(&repl("r4", "undefined", &numbered(&big)), &policy)
            .output
            .is_empty(),
        "second inner Read: full text"
    );

    // Async eval: the REPL only says "(dispatched #N)"; Poll delivers the
    // settled result XML-escaped (`&`, `<`, `>`) inside its event envelope.
    let esc = |t: &str| {
        t.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let dispatch = event(
        &s,
        "PostToolUse",
        &serde_json::json!({ "tool_use_id": "r5", "tool_name": "REPL",
            "tool_input": { "code": "…" },
            "tool_response": { "code": "…", "result": "(dispatched #4)", "stdout": "",
                "stderr": "", "evalId": 4, "asyncDispatched": true } }),
    );
    assert!(decide(&dispatch, &policy).output.is_empty());
    let late = code_file("late");
    let late_path = dir.path().join("late.rs");
    std::fs::write(&late_path, &late).unwrap();
    assert!(
        decide(&inner_read("repl_c3", &late_path, &late), &policy)
            .output
            .is_empty()
    );
    let envelope = format!(
        "<system>authentic event nonces: a1</system>\n<event nonce=\"a1\" kind=\"repl-eval\" \
         eval=\"4\" status=\"settled\"><result>{}</result></event>",
        esc(&numbered(&late))
    );
    let poll = serde_json::json!({
        "session_id": s, "tool_use_id": "toolu_p1", "hook_event_name": "PostToolUse",
        "tool_name": "Poll", "tool_input": {}, "tool_response": { "content": envelope },
    });
    let out = shaped(&decide(&poll.to_string(), &policy));
    let content = out["content"].as_str().unwrap();
    assert!(content.starts_with("<system>authentic event nonces: a1</system>\n<event "));
    assert!(content.ends_with("</result></event>"), "{content}");
    assert!(content.contains("[outline 540L"), "{content}");
    assert!(content.contains("(input:&amp;str)"), "{content}");
    assert!(!content.contains("&str"), "{content}");
}

/// Code Mode reads no hook sees: a script that reads a project file through a
/// REPL builtin and returns it whole hands the model R5's outline the first
/// time, the full text on a later read, and never touches a file outside the
/// session's project. Every file has its own content, so a view can only
/// match the file it was made for.
#[test]
fn code_mode_outlines_a_builtin_first_read_of_a_project_file_once() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_READ_DEDUP", "on");
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    let put = |name: &str| {
        let content = code_file(&name.replace(['/', '.'], "_"));
        std::fs::write(project.path().join(name), &content).unwrap();
        content
    };
    let big = put("src/big.rs");
    let foreign = outside.path().join("big.rs");
    let foreign_text = code_file("foreign");
    std::fs::write(&foreign, &foreign_text).unwrap();
    let s = unique_session("repl-first-read");
    let policy = Policy {
        r5: RuleMode::On,
        r6: RuleMode::Off,
        ..ALL_ON
    };
    let repl_with = |tool_use: &str, code: &str, stdout: &str| {
        serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "REPL", "cwd": project.path().to_string_lossy(),
            "tool_input": { "code": code },
            "tool_response": { "code": code, "result": "undefined", "stdout": stdout,
                "stderr": "", "toolsUsed": [] },
        })
        .to_string()
    };

    // Outside the project: never read, never shaped.
    let foreign_code = format!("console.log(await file('{}').text())", foreign.display());
    assert!(
        decide(&repl_with("r0", &foreign_code, &foreign_text), &policy)
            .output
            .is_empty()
    );

    let code = "console.log(await $`cat src/big.rs`.text())";
    let first = decide(&repl_with("r1", code, &big), &policy);
    assert_eq!(first.receipt.as_ref().map(|r| r.rule_id), Some("REPL"));
    let v: serde_json::Value = serde_json::from_str(&first.output).unwrap();
    let stdout = v["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
        .as_str()
        .unwrap();
    assert!(stdout.contains("[outline 540L"), "{stdout}");
    assert!(stdout.contains("src_big_rs_7"), "{stdout}");
    assert!(!stdout.contains("let trimmed"), "{stdout}");

    // The same file read again is the escape: full text.
    assert!(
        decide(&repl_with("r2", code, &big), &policy)
            .output
            .is_empty()
    );

    // A model `cat` of one whole project file gets the same rule.
    let cat = |tool_use: &str, cmd: &str, stdout: &str| {
        serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "Bash", "cwd": project.path().to_string_lossy(),
            "tool_input": { "command": cmd },
            "tool_response": { "stdout": stdout.trim_end(), "stderr": "",
                "interrupted": false, "isImage": false },
        })
        .to_string()
    };
    let cat_rs = put("src/cat.rs");
    std::fs::create_dir(project.path().join(".aws")).unwrap();
    let secret = put(".aws/keys.rs");
    // Over the 2 MiB bound, every line still distinct.
    let mut huge = String::new();
    for i in 0.. {
        if huge.len() > 2 * 1024 * 1024 {
            break;
        }
        huge.push_str(&code_file(&format!("huge{i}")));
    }
    std::fs::write(project.path().join("src/huge.rs"), &huge).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&foreign, project.path().join("src/link.rs")).unwrap();
    // Untouched, and no record spent: pipes, flags, several files, lists,
    // files outside the project (also through a symlink), secret-like paths,
    // output that is not the file, files over the size bound.
    for (cmd, stdout) in [
        ("cat src/cat.rs | head -1000".to_string(), &cat_rs),
        ("cat -n src/cat.rs".to_string(), &cat_rs),
        ("cat src/cat.rs src/big.rs".to_string(), &cat_rs),
        ("cd src && cat cat.rs".to_string(), &cat_rs),
        (format!("cat {}", foreign.display()), &foreign_text),
        ("cat src/link.rs".to_string(), &foreign_text),
        ("cat .aws/keys.rs".to_string(), &secret),
        ("cat src/cat.rs".to_string(), &big),
        ("cat src/huge.rs".to_string(), &huge),
    ] {
        let d = decide(&cat("b0", &cmd, stdout), &policy);
        assert!(d.output.is_empty(), "{cmd}");
        assert_ne!(d.receipt.map(|r| r.rule_id), Some("R5"), "{cmd}");
    }
    // A user-excluded command is a Passthrough: no first view.
    let excluded_cmd = "cat src/cat.rs";
    let ev: serde_json::Value = serde_json::from_str(&cat("b0", excluded_cmd, &cat_rs)).unwrap();
    assert!(
        super::bash::r5_cat(
            &ev,
            &policy,
            excluded_cmd,
            cat_rs.trim_end(),
            &["cat".to_string()],
            &s
        )
        .is_none()
    );
    let first = decide(&cat("b1", "cat 'src/cat.rs'", &cat_rs), &policy);
    assert_eq!(first.receipt.as_ref().map(|r| r.rule_id), Some("R5"));
    let stdout = replaced_stdout(&first).expect("outlined");
    assert!(stdout.contains("[outline 540L"), "{stdout}");
    assert!(!stdout.contains("let trimmed"), "{stdout}");
    // Duplicate fire: same answer; a later cat is the escape.
    assert_eq!(
        decide(&cat("b1", "cat 'src/cat.rs'", &cat_rs), &policy).output,
        first.output
    );
    assert!(
        decide(&cat("b2", "cat src/cat.rs", &cat_rs), &policy)
            .output
            .is_empty()
    );

    // Code Mode, async: the dispatch keeps the script; an inner `cat` keeps
    // the file for the script; the view reaches the model where the eval
    // hands the text back, also as console output in a Poll envelope's
    // `<log>`.
    let esc = |t: &str| {
        t.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let dispatch = |tool_use: &str, eval: u64, code: &str| {
        let ev = serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "REPL", "cwd": project.path().to_string_lossy(),
            "tool_input": { "code": code },
            "tool_response": { "code": code, "result": format!("(dispatched #{eval})"),
                "stdout": "", "stderr": "", "evalId": eval, "asyncDispatched": true },
        });
        assert!(decide(&ev.to_string(), &policy).output.is_empty());
    };
    let poll = |tool_use: &str, eval: u64, segments: &str| {
        let ev = serde_json::json!({
            "session_id": s, "tool_use_id": tool_use, "hook_event_name": "PostToolUse",
            "tool_name": "Poll", "cwd": project.path().to_string_lossy(),
            "tool_input": {}, "tool_response": { "content": format!(
                "<event nonce=\"a1\" kind=\"repl-eval\" eval=\"{eval}\" status=\"settled\">\n{segments}\n</event>") },
        });
        decide(&ev.to_string(), &policy)
    };
    let content_of = |d: &Decision| -> String {
        assert_eq!(d.receipt.as_ref().map(|r| r.rule_id), Some("REPL"));
        let v: serde_json::Value = serde_json::from_str(&d.output).unwrap();
        v["hookSpecificOutput"]["updatedToolOutput"]["content"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let inner_rs = put("src/inner.rs");
    dispatch("r3", 1, "self.log(await $`cat src/inner.rs`.text())");
    let inner = decide(&cat("repl_b3", "cat src/inner.rs", &inner_rs), &policy);
    assert!(inner.output.is_empty());
    let content = content_of(&poll(
        "toolu_p2",
        1,
        &format!("<log>{}</log>\n<result/>", esc(inner_rs.trim_end())),
    ));
    assert!(content.contains("[outline 540L"), "{content}");
    assert!(!content.contains("let trimmed"), "{content}");
    assert!(
        content.ends_with("</log>\n<result/>\n</event>"),
        "{content}"
    );
    // A second inner `cat` is the escape, and so is a builtin read after it:
    // the eval hands back the full text.
    let again = decide(&cat("repl_b4", "cat src/inner.rs", &inner_rs), &policy);
    assert!(again.output.is_empty());
    assert!(
        decide(
            &repl_with(
                "r4",
                "console.log(await $`cat src/inner.rs`.text())",
                &inner_rs
            ),
            &policy
        )
        .output
        .is_empty(),
        "second inner cat: full text"
    );
    assert!(
        decide(
            &repl_with(
                "r5",
                "console.log(await file('src/inner.rs').text())",
                &inner_rs
            ),
            &policy
        )
        .output
        .is_empty(),
        "builtin read after an inner cat: full text"
    );

    // Builtin reads that settle through Poll, in the shapes benchmark scripts
    // return (session replays 2026-11-20): the file joined with other text,
    // a JSON object field (escaped newlines), and a slice of it, which stays
    // untouched. The paths come from the dispatched script.
    let joined_rs = put("src/joined.rs");
    dispatch(
        "r6",
        2,
        "const a = await file('src/joined.rs').text(); a + '!'",
    );
    let file = joined_rs.trim_end();
    let joined = content_of(&poll(
        "toolu_p3",
        2,
        &format!(
            "<result>{}</result>",
            esc(&format!("{file}\n=====BASH=====\nLINES 540"))
        ),
    ));
    assert!(joined.contains("[outline 540L"), "{joined}");
    assert!(
        joined.contains("\n=====BASH=====\nLINES 540</result>"),
        "{joined}"
    );
    assert!(!joined.contains("let trimmed"), "{joined}");

    let object_rs = put("src/object.rs");
    dispatch("r7", 3, "({ a: await file('src/object.rs').text() })");
    let object = serde_json::json!({ "a": object_rs.trim_end(), "lines": 540 }).to_string();
    let field = content_of(&poll(
        "toolu_p4",
        3,
        &format!("<result>{}</result>", esc(&object)),
    ));
    assert!(field.contains("[outline 540L"), "{field}");
    assert!(field.contains("\"lines\":540}"), "{field}");
    assert!(!field.contains("let trimmed"), "{field}");

    let cut_rs = put("src/cut.rs");
    dispatch("r8", 5, "(await file('src/cut.rs').text()).slice(0, 9000)");
    let cut = &cut_rs[..cut_rs.len() / 2];
    assert!(
        poll("toolu_p5", 5, &format!("<result>{}</result>", esc(cut)))
            .output
            .is_empty()
    );
}

fn grep_payload(session: &str, response: &serde_json::Value) -> String {
    serde_json::json!({
        "session_id": session,
        "tool_use_id": "toolu_grep",
        "hook_event_name": "PostToolUse",
        "tool_name": "Grep",
        "cwd": "/repo",
        "tool_input": { "pattern": "handle_", "output_mode": "content", "-n": true },
        "tool_response": response,
    })
    .to_string()
}

/// A Grep content result as Claude Code returns it: matches with `-n` and
/// context lines, in long paths that repeat on every line.
fn grep_content() -> String {
    let mut s = String::new();
    for f in 0..12 {
        let path = format!("rust/crates/lean-ctx-hooks/src/hook_handlers/module_{f}/handler.rs");
        for l in 0..8 {
            let n = 10 + l * 7;
            s.push_str(&format!("{path}-{}-    // context before\n", n - 1));
            s.push_str(&format!("{path}:{n}:pub fn handle_{f}_{l}() {{}}\n"));
        }
        s.push_str("--\n");
    }
    s.trim_end_matches('\n').to_string()
}

fn grep_response(content: &str) -> serde_json::Value {
    serde_json::json!({
        "mode": "content", "numFiles": 0, "filenames": [],
        "content": content, "numLines": content.lines().count()
    })
}

/// Seamit bench: under the governor (`hook rewrite-enforce` PreToolUse) no
/// native Grep was ever shaped. R6 now regroups a large content result
/// losslessly, and leaves the other modes alone.
#[test]
fn r6_regroups_a_large_grep_content_result_losslessly() {
    let s = unique_session("r6");
    let content = grep_content();
    let d = decide(&grep_payload(&s, &grep_response(&content)), &ALL_ON);
    assert!(!d.output.is_empty(), "Grep content result rewritten");
    let out: serde_json::Value = serde_json::from_str(&d.output).unwrap();
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let updated = &out["hookSpecificOutput"]["updatedToolOutput"];
    assert_eq!(updated["mode"], "content", "other fields kept");
    let view = updated["content"].as_str().unwrap();
    assert_eq!(grep::expand(view), content, "lossless");
    assert!(
        view.starts_with("rust/crates/lean-ctx-hooks/src/hook_handlers/module_0/handler.rs\n9-")
    );
    assert_eq!(
        view.matches("module_3/handler.rs").count(),
        1,
        "each path once"
    );
    let r = d.receipt.as_ref().unwrap();
    assert_eq!((r.rule_id, r.mode), ("R6", RuleMode::On));
    assert!(r.actual_tokens < r.baseline_tokens);
    assert!(d.applied_stats().is_some(), "counted as a governor saving");

    // Deterministic (#498): the same result gives the same bytes.
    let again = decide(&grep_payload(&s, &grep_response(&content)), &ALL_ON);
    assert_eq!(again.output, d.output);
}

#[test]
fn r6_respects_its_mode_and_leaves_other_results_alone() {
    let s = unique_session("r6-modes");
    let content = grep_content();
    let payload = grep_payload(&s, &grep_response(&content));
    let with = |r6| Policy { r6, ..ALL_ON };

    let shadow = decide(&payload, &with(RuleMode::Shadow));
    assert!(shadow.output.is_empty(), "shadow never replaces");
    assert_eq!(shadow.receipt.map(|r| r.rule_id), Some("R6"));
    let off = decide(&payload, &with(RuleMode::Off));
    assert!(off.output.is_empty() && off.receipt.is_none());

    let files = serde_json::json!({
        "mode": "files_with_matches", "numFiles": 2,
        "filenames": ["a.rs", "b.rs"], "content": content
    });
    let small = grep_response("src/a.rs:1:fn handle_a() {}\nsrc/a.rs:2:fn handle_b() {}");
    let unparsed = grep_response(&format!("{content}\nnot a grep line"));
    for response in [files, small, unparsed] {
        let d = decide(&grep_payload(&s, &response), &ALL_ON);
        assert!(d.output.is_empty() && d.receipt.is_none(), "{response}");
    }
}

#[test]
fn r6_heading_view_round_trips_edge_cases() {
    for content in [
        "a.rs:1:x\na.rs-2-y\n--\nb.rs:3:z\n",
        "C:\\repo\\a.rs:10:x:1:y\nC:\\repo\\a.rs-11-ctx",
        "a-1-b.rs:4:x\na-1-b.rs-5-ctx\na.rs:6:",
        "a.rs:3:at 10:15:30\na.rs-4-x-5-y",
    ] {
        let view = grep::heading_view(content).expect(content);
        assert_eq!(grep::expand(&view), content, "{content:?} → {view:?}");
    }
    for refused in [
        "--\na.rs:1:x",
        "plain text",
        "a.rs:1:x\n\na.rs:2:y",
        // A path reappearing after its group closed: not ripgrep's order.
        "a.rs:1:x\nb.rs:2:y\na.rs:3:z",
        // Several known paths fit one line (`a.rs` and `a.rs-1-b.rs`).
        "a.rs:1:x\na.rs-1-b.rs:2:y\na.rs-1-b.rs-3-z",
    ] {
        assert!(grep::heading_view(refused).is_none(), "{refused:?}");
    }
}

/// bd F1 (!643): a context line whose text contains `:N:` reads as a match
/// of a fake path `src/a.rs-9-// at 10`. That gave a fake heading, a wrong
/// line number and split the file in two groups, while `expand` still
/// round-tripped. R6 must leave such a result untouched.
#[test]
fn r6_refuses_a_context_line_that_looks_like_a_match() {
    let content = "src/a.rs-9-// at 10:15:30\nsrc/a.rs:10:fn handle() {}\nsrc/a.rs-11-}";
    assert!(
        grep::heading_view(content).is_none(),
        "ambiguous parse refused"
    );
    // Inside a large result the whole rewrite is refused, not just the line.
    let big = format!("{}\n--\n{content}", grep_content());
    assert!(grep::heading_view(&big).is_none());
    let s = unique_session("r6-f1");
    let d = decide(&grep_payload(&s, &grep_response(&big)), &ALL_ON);
    assert!(d.output.is_empty() && d.receipt.is_none());
}

/// Every heading is a real file of the result, once, in result order — also
/// for paths that contain dashes, digits, colons and spaces.
#[test]
fn r6_headings_are_exactly_the_result_paths() {
    let files = [
        "src/v-2-x.rs",
        "docs/2024-11-notes.md",
        "C:\\repo\\with space\\m.rs",
        "lib/a:b.rs",
    ];
    let mut content = String::new();
    for f in files {
        content.push_str(&format!("{f}-1-before\n{f}:2:hit 3:4\n{f}-3-after\n--\n"));
    }
    let content = content.trim_end_matches("--\n").to_string();
    let view = grep::heading_view(&content).expect("parses");
    let headings: Vec<&str> = view
        .split("\n\n")
        .map(|g| g.split('\n').next().unwrap())
        .collect();
    assert_eq!(headings, files);
    assert_eq!(grep::expand(&view), content);
}

/// R6 needs line numbers: a Grep run with `-n: false` stays untouched.
#[test]
fn r6_leaves_grep_without_line_numbers_alone() {
    let s = unique_session("r6-no-n");
    let content = grep_content();
    let mut payload: serde_json::Value =
        serde_json::from_str(&grep_payload(&s, &grep_response(&content))).unwrap();
    payload["tool_input"]["-n"] = serde_json::json!(false);
    let d = decide(&payload.to_string(), &ALL_ON);
    assert!(d.output.is_empty() && d.receipt.is_none());
}

/// The PreToolUse half stays security only (owner decision O-1): an
/// ordinary Bash or Grep call passes unchanged, so R6 / R2 / R4 in `hook
/// govern` are the only place native output is shaped.
#[test]
fn enforce_hook_passes_ordinary_bash_and_grep_through() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let grep = serde_json::json!({ "tool_name": "Grep", "hook_event_name": "PreToolUse",
        "transcript_path": "/t", "tool_input": { "pattern": "fn main", "path": "/repo/src" } })
    .to_string();
    assert_eq!(enforce::enforce_decision(&grep, "lean-ctx"), "{}");
    let bash = serde_json::json!({ "tool_name": "Bash", "hook_event_name": "PreToolUse",
        "transcript_path": "/t", "tool_input": { "command": "git status" } })
    .to_string();
    assert_eq!(enforce::enforce_decision(&bash, "lean-ctx"), "{}");
}
