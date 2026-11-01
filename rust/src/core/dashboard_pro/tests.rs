// SPDX-License-Identifier: Apache-2.0
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::bridge::{Env, handle_apply, handle_inputs};
use super::collect::RouteSource;
use super::effects::Notifier;
use super::request::{self, Op};
use super::state::ProState;
use super::wire::ProjectColumn;
use super::*;
use crate::core::knowledge::ProjectKnowledge;
use crate::core::memory_policy::MemoryPolicy;

/// The cockpit's computation as a fixed answer; records what it was asked.
struct FakeRuntime {
    answer: Value,
    asked: Mutex<Vec<Value>>,
}

impl FakeRuntime {
    fn planning(plans: &Value) -> Self {
        Self {
            answer: json!({ "status": 200, "body": null, "file": null, "plans": plans, "error": null }),
            asked: Mutex::new(Vec::new()),
        }
    }
}

struct Quiet;

impl Notifier for Quiet {
    fn desktop(&self, _text: &str) -> bool {
        false
    }
    fn webhook(&self, _url: &str, _body: &[u8]) -> bool {
        false
    }
}

impl RouteSource for Quiet {
    fn get(&self, _path: &str) -> Option<Value> {
        None
    }
}

fn project(data: &Path) -> (tempfile::TempDir, ProjectScope) {
    let dir = tempfile::tempdir().unwrap();
    let scope = ProjectScope::with_dirs(
        dir.path().to_str().unwrap(),
        data.to_path_buf(),
        data.to_path_buf(),
        None,
    )
    .unwrap();
    (dir, scope)
}

fn remember(scope: &ProjectScope, category: &str, key: &str, value: &str) {
    ProjectKnowledge::mutate_locked(&scope.root, |k| {
        k.remember(
            category,
            key,
            value,
            "fixture",
            0.9,
            &MemoryPolicy::default(),
        );
    })
    .unwrap();
}

fn current(scope: &ProjectScope) -> Vec<String> {
    collect::knowledge_records(scope)
        .iter()
        .filter(|f| f.is_current())
        .map(|f| format!("{}/{}", f.category, f.key))
        .collect()
}

fn env(scope: &ProjectScope) -> Env<'_> {
    Env {
        scope,
        routes: &Quiet,
        notifier: &Quiet,
        now: chrono::Utc::now(),
    }
}

/// One round as the cockpit runs it: inputs, the computation, apply.
fn ask(scope: &ProjectScope, runtime: &FakeRuntime, request: &Value) -> bridge::Reply {
    let env = env(scope);
    let inputs = handle_inputs(&env, &request.to_string());
    if inputs.status != 200 {
        return inputs;
    }
    runtime
        .asked
        .lock()
        .unwrap()
        .push(inputs.body["request"].clone());
    let mut apply = request.clone();
    apply["response"] = runtime.answer.clone();
    handle_apply(&env, &apply.to_string())
}

#[test]
fn plans_outside_the_project_or_the_request_are_refused_with_409() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (dir, scope) = project(iso.path());
    std::fs::write(dir.path().join("inside.rs"), "fn main() {}").unwrap();
    remember(&scope, "arch", "db", "postgres");

    // A path that leaves the project, relative or absolute.
    for path in ["../escape.rs", "/etc/hosts"] {
        let runtime =
            FakeRuntime::planning(&json!([{ "effect": "adaptive.dismiss", "path": path }]));
        let reply = ask(
            &scope,
            &runtime,
            &json!({ "op": "adaptive.dismiss", "method": "POST", "body": { "path": path } }),
        );
        assert_eq!(reply.status, 409, "{path}: {}", reply.body);
        assert!(ProState::new(&scope).adaptive().dismissed.is_empty());
    }

    // An overlay on a file outside the project is never written.
    let runtime = FakeRuntime::planning(&json!([
        { "effect": "context.overlay", "action": "exclude", "path": "/etc/hosts" }
    ]));
    let reply = ask(
        &scope,
        &runtime,
        &json!({ "op": "adaptive.apply", "method": "POST", "body": { "path": "/etc/hosts", "action": "exclude" } }),
    );
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(
        crate::core::context_overlay::OverlayStore::load_project(dir.path())
            .all()
            .is_empty()
    );

    // An unknown effect, and a known effect the op does not allow.
    for plans in [
        json!([{ "effect": "shell.exec", "command": "rm -rf /" }]),
        json!([{ "effect": "memory.forget", "refs": ["arch/db"], "reason": "x" }]),
    ] {
        let runtime = FakeRuntime::planning(&plans);
        let reply = ask(
            &scope,
            &runtime,
            &json!({ "op": "alerts.update", "method": "POST", "body": { "enabled": true } }),
        );
        assert_eq!(reply.status, 409, "{}", reply.body);
    }
    assert_eq!(current(&scope), vec!["arch/db".to_string()]);
    assert!(!ProState::new(&scope).alert_settings().enabled);
}

#[test]
fn forget_snapshots_first_and_removes_only_this_projects_facts() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_a, mine) = project(iso.path());
    let (_b, other) = project(iso.path());
    remember(&mine, "arch", "db", "postgres");
    remember(&mine, "arch", "cache", "redis");
    remember(&other, "arch", "db", "mysql");
    remember(&other, "deploy", "host", "eu-west");

    let forget = |refs: Value| {
        let runtime = FakeRuntime::planning(&json!([
            { "effect": "memory.forget", "refs": refs, "reason": "Before cleanup" }
        ]));
        ask(
            &mine,
            &runtime,
            &json!({ "op": "memory.forget", "method": "POST", "body": { "refs": refs } }),
        )
    };

    let reply = forget(json!(["arch/db"]));
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(current(&mine), vec!["arch/cache".to_string()]);
    assert_eq!(current(&other).len(), 2, "another project's same key stays");

    let saved = snapshots::list(&mine, 30);
    assert_eq!(saved.len(), 1);
    let safety = snapshots::read_facts(&mine, &saved[0].id).unwrap();
    assert!(
        safety
            .iter()
            .any(|f| f.key == "db" && f.value == "postgres")
    );
    assert_eq!(
        reply.body["applied"][0]["readback"]["safety_snapshot"],
        json!(saved[0].id)
    );

    // A fact only the other project holds is not this project's to forget.
    let reply = forget(json!(["deploy/host"]));
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(current(&other).len(), 2);
    assert_eq!(
        snapshots::list(&mine, 30).len(),
        1,
        "no snapshot for a refused plan"
    );
}

#[test]
fn restore_is_not_limited_by_a_fact_count() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_dir, scope) = project(iso.path());
    for key in ["a", "b", "c"] {
        remember(&scope, "arch", key, &format!("value {key}"));
    }
    let knowledge = ProjectKnowledge::load_or_create(&scope.root);
    let snapshot = snapshots::create(&scope, &knowledge, "fixture", chrono::Utc::now()).unwrap();
    ProjectKnowledge::mutate_locked(&scope.root, |k| {
        k.remove_fact("arch", "a");
        k.remove_fact("arch", "b");
    })
    .unwrap();

    let restore = || {
        let runtime = FakeRuntime::planning(&json!([{
            "effect": "memory.restore", "id": snapshot.id, "missing": ["arch/a", "arch/b"], "changed": []
        }]));
        ask(
            &scope,
            &runtime,
            &json!({ "op": "memory.restore", "method": "POST", "body": { "id": snapshot.id, "confirm": true } }),
        )
    };

    // Knowledge has no fact limit: the legacy cap env var no longer refuses
    // a restore that grows the store past it.
    crate::test_env::set_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS", "2");
    let restored = restore();
    crate::test_env::remove_var("LEAN_CTX_KNOWLEDGE_MAX_FACTS");
    assert_eq!(restored.status, 200, "{}", restored.body);
    let mut back = current(&scope);
    back.sort();
    assert_eq!(back, ["arch/a", "arch/b", "arch/c"]);
}

#[test]
fn ledger_export_neutralises_spreadsheet_formulas() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_dir, scope) = project(iso.path());
    let ledger = ledger::ledger_path(&scope);
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    let entry = json!({
        "ts": "2026-10-01T10:00:00Z", "tool": "=HYPERLINK(\"http://x\")", "mechanism": "+cmd|' /C calc'!A0",
        "model_id": "@SUM(1)", "saved_tokens": 5, "baseline_tokens": 10, "repo_hash": scope.repo_hashes[0],
        "measurement_method": "-2+3", "prev_hash": "genesis", "entry_hash": "h1"
    });
    std::fs::write(&ledger, format!("{entry}\n")).unwrap();
    let filters = ledger::filters(&scope, &serde_json::Map::new()).unwrap();
    let column = ProjectColumn::Fixed {
        name: scope.label.clone(),
    };
    let csv = String::from_utf8(export::build(&scope, &filters, "csv", &column).unwrap()).unwrap();
    let row = csv.lines().nth(1).expect("one entry");
    for cell in row.split(',') {
        let cell = cell.trim_start_matches('"');
        assert!(
            !cell.starts_with(['=', '+', '-', '@']),
            "formula cell {cell:?} in {row}"
        );
    }
    assert!(row.contains("'=HYPERLINK"), "{row}");
    assert!(row.contains("'@SUM(1)"), "{row}");
}

#[test]
fn apply_refuses_a_missing_answer_and_foreign_fields() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_dir, scope) = project(iso.path());
    let env = env(&scope);
    for raw in [
        json!({ "op": "memory.analyze", "method": "GET" }),
        json!({ "op": "memory.analyze", "method": "GET", "response": "text" }),
        json!({ "op": "memory.analyze", "method": "GET", "response": { "status": 200 }, "guard_cursor": 0 }),
        json!({ "op": "alerts.evaluate", "method": "POST", "response": { "status": 200 } }),
    ] {
        assert_eq!(handle_apply(&env, &raw.to_string()).status, 400, "{raw}");
    }
    let refused = handle_apply(
        &env,
        &json!({ "op": "memory.analyze", "method": "GET", "response": { "status": 409, "error": { "code": "x", "message": "Not now" } } })
            .to_string(),
    );
    assert_eq!(
        (refused.status, refused.body),
        (409, json!({ "error": "Not now" }))
    );
}

#[test]
fn a_projects_guard_timeline_counts_only_its_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let trail = dir.path().join("trail.jsonl");
    let line = |kind: &str, session: &str| {
        format!(
            r#"{{"timestamp":"2026-10-20T10:00:00Z","tool":"ctx_shell","action":"{kind}:1{session}"}}"#
        )
    };
    let lines = [
        line("shell_blocked", "|session=mine"),
        line("path_blocked", "|session=theirs"),
        line("shell_blocked", ""),
    ];
    std::fs::write(&trail, lines.join("\n") + "\n").unwrap();
    let count = |days: &[Value], kind: &str| {
        days.iter()
            .map(|row| row[kind].as_i64().unwrap_or(0))
            .sum::<i64>()
    };

    let (device, recent) = guard::timeline(&trail).unwrap();
    assert_eq!(count(&device, "shell_blocked"), 2);
    assert_eq!(count(&device, "path_blocked"), 1);
    assert_eq!(recent.len(), 3);

    let mine = std::collections::HashSet::from(["mine".to_string()]);
    let (days, recent) = guard::timeline_for(&trail, Some(&mine)).unwrap();
    assert_eq!(
        count(&days, "shell_blocked"),
        1,
        "an entry without a session is no project's"
    );
    assert_eq!(count(&days, "path_blocked"), 0, "another project's session");
    assert_eq!(recent.len(), 1);

    // The device timeline is unchanged by the project's.
    let (device, _) = guard::timeline(&trail).unwrap();
    assert_eq!(count(&device, "shell_blocked"), 2);
}

#[test]
fn the_alerts_cursor_moves_only_after_an_answered_evaluation() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_dir, scope) = project(iso.path());
    let env = env(&scope);
    let evaluate = json!({ "op": "alerts.evaluate", "method": "POST" });
    let inputs = || handle_inputs(&env, &evaluate.to_string());

    // Alerts off: nothing to evaluate.
    assert_eq!(inputs().body, json!({ "skip": true }));

    ProState::new(&scope)
        .put_alert_settings(&wire::AlertSettings {
            enabled: true,
            kinds: vec!["shell_blocked".into()],
            desktop: false,
            webhook: String::new(),
        })
        .unwrap();
    let trail = scope.data_dir.join("audit").join("trail.jsonl");
    std::fs::create_dir_all(trail.parent().unwrap()).unwrap();
    let event =
        r#"{"timestamp":"2026-10-20T10:00:00Z","tool":"ctx_shell","action":"shell_blocked:1"}"#;
    std::fs::write(&trail, format!("{event}\n")).unwrap();

    // The first look marks where alerting starts; history never alerts.
    assert_eq!(inputs().body, json!({ "skip": true }));
    let start = ProState::new(&scope).guard_cursor().unwrap();
    assert_eq!(start, std::fs::metadata(&trail).unwrap().len());

    let mut appended = std::fs::OpenOptions::new()
        .append(true)
        .open(&trail)
        .unwrap();
    std::io::Write::write_all(&mut appended, format!("{event}\n").as_bytes()).unwrap();
    let fresh = inputs();
    assert_eq!(fresh.status, 200, "{}", fresh.body);
    assert_eq!(
        fresh.body["request"]["inputs"]["guard_new"][0]["kind"],
        json!("shell_blocked")
    );
    let end = fresh.body["guard_cursor"].as_u64().unwrap();
    assert!(end > start);

    // A failed evaluation leaves the cursor: the events come again.
    let mut apply = evaluate.clone();
    apply["guard_cursor"] = json!(end);
    apply["response"] = json!({ "status": 502 });
    assert_eq!(handle_apply(&env, &apply.to_string()).status, 502);
    assert_eq!(ProState::new(&scope).guard_cursor(), Some(start));
    assert_eq!(inputs().body["guard_cursor"], json!(end));

    // A cursor past the trail is refused.
    apply["response"] = json!({ "status": 200, "body": null });
    apply["guard_cursor"] = json!(end + 1);
    assert_eq!(handle_apply(&env, &apply.to_string()).status, 400);

    apply["guard_cursor"] = json!(end);
    assert_eq!(handle_apply(&env, &apply.to_string()).status, 200);
    assert_eq!(ProState::new(&scope).guard_cursor(), Some(end));
    assert_eq!(inputs().body, json!({ "skip": true }));
}

/// A route answer larger than one exchange frame.
struct HugeRoutes;

impl RouteSource for HugeRoutes {
    fn get(&self, _path: &str) -> Option<Value> {
        let edge = json!({ "from": "a/x", "to": "a/y", "kind": "contradicts" });
        Some(json!({ "edges": vec![edge; 30_000] }))
    }
}

#[test]
fn an_input_over_one_frame_is_413() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_dir, scope) = project(iso.path());
    let env = Env {
        scope: &scope,
        routes: &HugeRoutes,
        notifier: &Quiet,
        now: chrono::Utc::now(),
    };
    let reply = handle_inputs(&env, r#"{"op":"memory.analyze","method":"GET"}"#);
    assert_eq!(reply.status, 413, "{}", reply.body);
}

#[test]
fn collectors_never_read_another_projects_partition() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_a, mine) = project(iso.path());
    let (_b, other) = project(iso.path());
    remember(&other, "arch", "db", "mysql");
    let knowledge = ProjectKnowledge::load_or_create(&other.root);
    let foreign = snapshots::create(&other, &knowledge, "other", chrono::Utc::now()).unwrap();
    ProState::new(&other)
        .put_alert_settings(&wire::AlertSettings {
            enabled: true,
            kinds: vec!["shell_blocked".into()],
            desktop: false,
            webhook: "https://hooks.example/other".into(),
        })
        .unwrap();

    let inputs = |op: &str, body: Value, query: Value| {
        let request = request::parse(
            &json!({ "op": op, "method": if body.is_null() { "GET" } else { "POST" }, "query": query, "body": body })
                .to_string(),
        )
        .unwrap();
        collect::inputs(&request, &mine, &Quiet, chrono::Utc::now(), None)
    };
    let analyzed = inputs("memory.analyze", Value::Null, json!({})).unwrap();
    assert_eq!(analyzed.inputs["snapshots"], json!([]));
    assert_eq!(analyzed.inputs["facts"], json!([]));

    let compared = inputs("memory.compare", Value::Null, json!({ "id": foreign.id }));
    assert_eq!(compared.err().map(|e| e.status), Some(404));

    let alerts = inputs("alerts.test", json!({}), json!({})).unwrap();
    assert_eq!(
        alerts.inputs["alerts_state"]["settings"]["enabled"],
        json!(false)
    );
    assert_eq!(
        alerts.inputs["alerts_state"]["settings"]["webhook"],
        json!("")
    );

    // The request names only this project's folder hashes.
    let runtime = FakeRuntime {
        answer: json!({ "status": 200, "body": {}, "file": null, "plans": null, "error": null }),
        asked: Mutex::new(Vec::new()),
    };
    let _ = ask(
        &mine,
        &runtime,
        &json!({ "op": "memory.analyze", "method": "GET" }),
    );
    let asked = runtime.asked.lock().unwrap();
    assert_eq!(asked[0]["project"]["repo_hashes"], json!(mine.repo_hashes));
    assert_eq!(asked[0]["op"], json!(Op::MemoryAnalyze.name()));
}

/// Two projects' reductions and one proxy cache read, as the ledger holds them.
fn three_entry_ledger(data: &Path, mine: &str, other: &str) {
    let path = data.join("savings").join("ledger.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let lines: Vec<String> = [
        ("ctx_read", "compression", mine),
        ("ctx_read", "compression", other),
        ("proxy_cache", "caching", "8a5edab282632443"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (tool, mechanism, repo))| {
        json!({
            "ts": "2026-10-01T10:00:00Z", "tool": tool, "mechanism": mechanism,
            "model_id": "claude-opus-5-5", "saved_tokens": 5, "baseline_tokens": 10,
            "repo_hash": repo, "prev_hash": "genesis", "entry_hash": format!("h{i}")
        })
        .to_string()
    })
    .collect();
    std::fs::write(path, lines.join("\n") + "\n").unwrap();
}

fn browsed(scope: &ProjectScope) -> i64 {
    let filters = ledger::filters(scope, &serde_json::Map::new()).unwrap();
    let (page, _, _) = ledger::browse(scope, &filters, 0, 50).unwrap();
    page["total"].as_i64().unwrap()
}

#[test]
fn a_project_ledger_holds_its_own_proxy_cache_reads_only() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (_dir, mine) = project(iso.path());
    three_entry_ledger(iso.path(), &mine.repo_hashes[0], "00000000000000ff");
    // Its own reduction; neither the other project's nor the older proxy
    // read that names no project.
    assert_eq!(browsed(&mine), 1);
    // A proxy read recorded with the project's folder counts for it.
    let path = iso.path().join("savings").join("ledger.jsonl");
    let mut ledger = std::fs::read_to_string(&path).unwrap();
    ledger.push_str(
        &(json!({
            "ts": "2026-10-02T10:00:00Z", "tool": "proxy_cache", "mechanism": "caching",
            "model_id": "claude-opus-5-5", "saved_tokens": 0, "baseline_tokens": 10,
            "repo_hash": mine.repo_hashes[0], "prev_hash": "h2", "entry_hash": "h3"
        })
        .to_string()
            + "\n"),
    );
    std::fs::write(&path, ledger).unwrap();
    assert_eq!(browsed(&mine), 2);
    let days = ledger::proxy_days(&mine).unwrap();
    assert_eq!(days.days.len(), 1, "only the project's own proxy read");
}

#[test]
fn the_device_scope_reads_every_project_and_names_each() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let (one, mine) = project(iso.path());
    let other = tempfile::tempdir().unwrap();
    let other_root = other.path().to_str().unwrap().to_string();
    three_entry_ledger(iso.path(), &mine.repo_hashes[0], &repo_hash(&other_root));
    let device = ProjectScope::device_with_dirs(
        &[mine.root.clone(), other_root.clone()],
        iso.path().to_path_buf(),
        iso.path().to_path_buf(),
        None,
    );
    assert!(device.is_device());
    assert_eq!(browsed(&device), 3);
    // The runtime reads empty folder hashes as "not narrowed".
    let wire = device.project();
    assert!(wire.repo_hashes.is_empty());
    let named: Vec<&str> = wire
        .labels
        .iter()
        .filter(|label| label.hash == repo_hash(&other_root))
        .map(|label| label.name.as_str())
        .collect();
    assert_eq!(named, [other.path().file_name().unwrap().to_str().unwrap()]);
    // A project filter names one project's folders among all of them.
    let mut query = serde_json::Map::new();
    query.insert("project".into(), json!(named[0]));
    let filters = ledger::filters(&device, &query).unwrap();
    let (page, _, _) = ledger::browse(&device, &filters, 0, 50).unwrap();
    assert_eq!(page["total"], json!(1));
    // Nothing inside a project is reachable from the device scope.
    assert!(device.jailed("src/main.rs").is_err());
    drop(one);
}

#[test]
fn the_bridge_refuses_a_project_op_in_the_device_scope() {
    let iso = crate::core::data_dir::isolated_data_dir();
    let device = ProjectScope::device_with_dirs(
        &[],
        iso.path().to_path_buf(),
        iso.path().to_path_buf(),
        None,
    );
    let env = env(&device);
    let memory = json!({ "op": "memory.analyze", "method": "GET" }).to_string();
    assert_eq!(handle_inputs(&env, &memory).status, 400);
    let mut apply = json!({ "op": "memory.forget", "method": "POST" });
    apply["response"] =
        json!({ "status": 200, "body": null, "file": null, "plans": null, "error": null });
    assert_eq!(handle_apply(&env, &apply.to_string()).status, 400);
    // A device-wide op is answered.
    let ledger = json!({ "op": "ledger.browse", "method": "GET" }).to_string();
    assert_eq!(handle_inputs(&env, &ledger).status, 200);
}

#[test]
fn only_device_wide_ops_answer_all_projects() {
    for op in [
        Op::SecurityReport,
        Op::ReportBuild,
        Op::LedgerBrowse,
        Op::LedgerExport,
        Op::SpendTrend,
        Op::AlertsUpdate,
        Op::AlertsTest,
        Op::AlertsEvaluate,
        Op::Profiles,
    ] {
        assert!(op.device_wide(), "{}", op.name());
    }
    for op in [
        Op::MemoryAnalyze,
        Op::MemoryRestore,
        Op::MemoryForget,
        Op::MemoryConsolidate,
        Op::WorkAnalysis,
        Op::AdaptiveState,
        Op::AdaptiveApply,
    ] {
        assert!(!op.device_wide(), "{}", op.name());
    }
}
