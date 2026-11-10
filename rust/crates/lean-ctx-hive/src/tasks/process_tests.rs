// SPDX-License-Identifier: Apache-2.0
//! Submit and verify of process tasks; checker ≠ author by the per-session
//! identity, never the machine's shared worker.

use super::*;
use crate::tasks::claims::{SessionIdentity, WorkTrace};
use crate::tasks::gate::tests::{
    Repo, cargo_command, cargo_locked, dirs, green_cargo_repo, locked, spec_repo, work,
};
use lean_ctx_protocol::hive::gate::{RULE_LOCKED_PATHS_CHANGED, TaskProcessV1, TaskRisk};
use serde_json::{Value, json};

/// The machine worker every session on a shared host appears as in Hive.
const SHARED_WORKER: &str = "wkr_ic7yo2evmqb6zhrg6v23";

fn task(state: &str, by: Value, process: Option<Value>, commit: &str) -> TaskV1 {
    let mut task = json!({
        "schema": "leanctx.hive.task/v1",
        "vault_id": "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42",
        "number": 9,
        "state": state,
        "origin": {"kind": "agent"},
        "created_by": {"kind": "person", "label": "Ada", "mine": false},
        "created_at": "2026-11-05T09:00:00Z",
    });
    if state == "submitted" {
        task["result"] = json!({
            "by": by, "at": "2026-11-05T10:00:00Z", "proposal": 0,
            "evidence": [{"kind": "commit", "reference": commit, "verified": false}],
        });
    }
    if let Some(process) = process {
        task["process"] = process;
    }
    serde_json::from_value(task).unwrap()
}

fn this_instance() -> Value {
    json!({"kind": "instance", "label": "pounce", "agent": SHARED_WORKER, "mine": true})
}

/// A session whose agent was registered by its own process.
fn session(agent: &str, app: Option<&str>) -> SessionIdentity {
    SessionIdentity {
        agent: agent.to_owned(),
        session: app.map(str::to_owned),
        process: Some(format!("proc-of-{agent}")),
    }
}

/// A live presence as the local registry would answer it.
fn presence(agent: &str, role: &str, bound_here: bool) -> Presence {
    Presence {
        agent: agent.to_owned(),
        role: Some(role.to_owned()),
        app_session: None,
        process: Some(format!("proc-of-{agent}")),
        bound_here,
    }
}

fn trace(claimed: &str, by: Option<SessionIdentity>, start: Option<&str>) -> WorkTrace {
    WorkTrace {
        vault_id: "3f6c1e9a0b7d4c2e8f5a1b6c9d0e7f42".into(),
        number: 9,
        claimed_by: claimed.into(),
        claimed_session: None,
        start_commit: start.map(str::to_owned),
        submitted_by: by,
        submitted_commit: None,
    }
}

fn commit_evidence(sha: &str) -> Vec<EvidenceV1> {
    vec![EvidenceV1 {
        kind: EvidenceKind::Commit,
        reference: Some(sha.to_owned()),
        anchor: None,
    }]
}

fn m_process(spec: &str, command: &str) -> Value {
    json!({"risk": "M", "spec_commit": spec, "locked_paths": locked(),
        "test_command": command, "features": ["F1"]})
}

/// An M process whose command is a real cargo run: only a run that writes
/// the harness report can pass an M gate (review r5 R5-3).
fn m_cargo_process(spec: &str) -> Value {
    json!({"risk": "M", "spec_commit": spec, "locked_paths": cargo_locked(),
        "test_command": cargo_command(), "features": ["F1"]})
}

fn refusal(err: HiveError) -> String {
    match err {
        HiveError::Refused(text) => text,
        other => panic!("not a refusal: {other:?}"),
    }
}

// --- checker ≠ author -------------------------------------------------

/// Queen 06.11.: two sessions on one Instance share the machine worker in
/// Hive; the worker may verify what the reviewer did not write.
#[test]
fn two_sessions_on_one_instance_verify_each_other() {
    let worker = session("mcp-100-worker", Some("ses-worker"));
    let reviewer = session("mcp-200-reviewer", Some("ses-reviewer"));
    let submitted = task("submitted", this_instance(), None, "9f01cf32a3");
    let t = trace(&worker.agent, Some(worker.clone()), None);
    checker_not_author(&submitted, &reviewer, Some(&t)).unwrap();
}

#[test]
fn the_same_session_never_verifies_its_own_result() {
    let worker = session("mcp-100-worker", Some("ses-worker"));
    let submitted = task("submitted", this_instance(), None, "9f01cf32a3");
    let t = trace(&worker.agent, Some(worker.clone()), None);
    let text = refusal(checker_not_author(&submitted, &worker, Some(&t)).unwrap_err());
    assert!(text.starts_with("self_check"), "{text}");
    // The same app session under a new bus agent (an MCP restart) too.
    let restarted = session("mcp-101-worker", Some("ses-worker"));
    assert!(checker_not_author(&submitted, &restarted, Some(&t)).is_err());
    // The session that claimed the task never checks it either.
    let claimant = trace("mcp-300-claimant", Some(worker), None);
    assert!(
        checker_not_author(
            &submitted,
            &session("mcp-300-claimant", None),
            Some(&claimant)
        )
        .is_err()
    );
}

#[test]
fn no_per_session_identity_fails_closed() {
    // The verifier names only the machine's shared worker.
    let text = refusal(
        checker_identity(
            SHARED_WORKER,
            Some(presence(SHARED_WORKER, "tester", true)),
            false,
        )
        .unwrap_err(),
    );
    assert!(text.starts_with("no per-session identity"), "{text}");
    assert!(checker_identity("  ", None, false).is_err());
    let reviewer = presence("mcp-200-reviewer", "code_reviewer", true);
    assert_eq!(
        checker_identity("mcp-200-reviewer", Some(reviewer), true)
            .unwrap()
            .agent,
        "mcp-200-reviewer"
    );
    // A result of this Instance without a record of who submitted it is
    // never taken as "same agent", and never as someone else either.
    let submitted = task("submitted", this_instance(), None, "9f01cf32a3");
    let text = refusal(
        checker_not_author(&submitted, &session("mcp-200-reviewer", None), None).unwrap_err(),
    );
    assert!(text.starts_with("no per-session identity"), "{text}");
}

#[test]
fn another_instance_or_a_person_is_someone_else() {
    let reviewer = session("mcp-200-reviewer", None);
    let other = json!({"kind": "instance", "label": "mac", "agent": SHARED_WORKER, "mine": false});
    checker_not_author(
        &task("submitted", other, None, "9f01cf32a3"),
        &reviewer,
        None,
    )
    .unwrap();
    let person = json!({"kind": "person", "label": "Ada", "mine": false});
    checker_not_author(
        &task("submitted", person, None, "9f01cf32a3"),
        &reviewer,
        None,
    )
    .unwrap();
    let unknown = json!({"kind": "robot", "label": "x", "mine": false});
    assert!(
        checker_not_author(
            &task("submitted", unknown, None, "9f01cf32a3"),
            &reviewer,
            None
        )
        .is_err()
    );
    assert!(checker_not_author(&task("open", Value::Null, None, ""), &reviewer, None).is_err());
}

// --- submit -----------------------------------------------------------

#[test]
fn a_task_that_is_not_a_process_task_submits_as_before() {
    let (repo, spec) = spec_repo();
    let open = task("claimed", Value::Null, None, "");
    let none = submit_gate(
        &open,
        &repo.root,
        &dirs(&repo),
        &[],
        TaskOutcome::Completed,
        None,
    )
    .unwrap();
    assert!(none.is_none());
    let _ = spec;
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the G6 gate run is Unix-only (passwd home, rustup PATH)"
)]
fn submit_with_a_green_g6_carries_the_record() {
    let (repo, spec, head) = green_cargo_repo();
    let t = task("claimed", Value::Null, Some(m_cargo_process(&spec)), "");
    let record = submit_gate(
        &t,
        &repo.root,
        &dirs(&repo),
        &commit_evidence(&head),
        TaskOutcome::Completed,
        None,
    )
    .unwrap()
    .expect("a process task carries its G6 record");
    assert!(record.ready_for_hive());
    assert_eq!(record.missing, vec!["ci_pipeline".to_owned()]);
    assert_eq!(record.artifact_commit, head);
    assert_eq!(record.rerun.as_ref().unwrap().commit, head);
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the G6 gate run is Unix-only (passwd home, rustup PATH)"
)]
fn submit_without_a_green_g6_is_refused() {
    let (repo, spec) = spec_repo();
    // A changed locked test: fail, rule locked_paths_changed, the path named.
    repo.write("tests/refund.rs", "weakened\n");
    let head = repo.commit("weaken");
    let t = task("claimed", Value::Null, Some(m_process(&spec, "true")), "");
    let text = refusal(
        submit_gate(
            &t,
            &repo.root,
            &dirs(&repo),
            &commit_evidence(&head),
            TaskOutcome::Completed,
            None,
        )
        .unwrap_err(),
    );
    assert!(text.contains(RULE_LOCKED_PATHS_CHANGED), "{text}");
    assert!(text.contains("tests/refund.rs modified"), "{text}");
    assert!(text.contains("not sent"), "{text}");

    // A red rerun.
    let (repo, spec) = spec_repo();
    let head = repo.commit("nothing");
    let t = task("claimed", Value::Null, Some(m_process(&spec, "exit 1")), "");
    let text = refusal(
        submit_gate(
            &t,
            &repo.root,
            &dirs(&repo),
            &commit_evidence(&head),
            TaskOutcome::Completed,
            None,
        )
        .unwrap_err(),
    );
    assert!(text.contains("G6 fail"), "{text}");

    // No commit named: nothing to judge.
    assert!(
        submit_gate(
            &t,
            &repo.root,
            &dirs(&repo),
            &[],
            TaskOutcome::Completed,
            None
        )
        .is_err()
    );
}

#[test]
fn a_view_without_what_g6_needs_is_not_supported_yet() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("work");
    for process in [
        json!({"risk": "M", "spec_commit": spec, "locked_paths": locked()}),
        json!({"risk": "H", "test_command": "true", "locked_paths": locked()}),
        json!({"risk": "H", "spec_commit": spec, "test_command": "true"}),
        json!({"risk": "S"}),
    ] {
        let t = task("claimed", Value::Null, Some(process), "");
        let start = trace("mcp-100-worker", None, Some(&spec));
        let text = refusal(
            submit_gate(
                &t,
                &repo.root,
                &dirs(&repo),
                &commit_evidence(&head),
                TaskOutcome::Completed,
                Some(&start),
            )
            .unwrap_err(),
        );
        assert!(text.starts_with("not supported by Hive yet"), "{text}");
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the G6 gate run is Unix-only (passwd home, rustup PATH)"
)]
fn an_s_task_pins_the_claims_start_commit_and_runs_the_repo_command() {
    let (repo, start) = spec_repo();
    repo.write("tests/refund.rs", "an S task has no locked tests\n");
    let head = repo.commit("fix");
    let t = task(
        "claimed",
        Value::Null,
        Some(json!({"risk": "S", "repo_test_command": "true"})),
        "",
    );
    let text = refusal(
        submit_gate(
            &t,
            &repo.root,
            &dirs(&repo),
            &commit_evidence(&head),
            TaskOutcome::Completed,
            None,
        )
        .unwrap_err(),
    );
    assert!(text.contains("claim the task again"), "{text}");
    let traced = trace("mcp-100-worker", None, Some(&start));
    let record = submit_gate(
        &t,
        &repo.root,
        &dirs(&repo),
        &commit_evidence(&head),
        TaskOutcome::Completed,
        Some(&traced),
    )
    .unwrap()
    .unwrap();
    assert_eq!(record.risk, TaskRisk::S);
    assert_eq!(record.locked_paths.spec_commit, start);
    assert!(record.locked_paths.checked.is_empty());
    assert_eq!(record.rerun.as_ref().unwrap().command, "true");
}

#[test]
fn g6_judges_the_commit_g4_passed() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("work");
    let mut process = m_process(&spec, "true");
    process["candidate_commit"] = spec.clone().into();
    let t = task("claimed", Value::Null, Some(process), "");
    let text = refusal(
        submit_gate(
            &t,
            &repo.root,
            &dirs(&repo),
            &commit_evidence(&head),
            TaskOutcome::Completed,
            None,
        )
        .unwrap_err(),
    );
    assert!(text.contains("G4 passed"), "{text}");
}

// --- verify -----------------------------------------------------------

/// The Instance's record turned into the one Hive stores: the contract
/// shape (`artifact`), a pass, and `ci` for the judged commit.
pub(crate) fn as_hive_record(instance: &Value, ci: Option<Value>) -> Value {
    let mut record = instance.clone();
    let obj = record.as_object_mut().unwrap();
    let commit = obj.remove("artifact_commit").unwrap();
    obj.insert("artifact".into(), json!({"kind": "commit", "ref": commit}));
    obj.insert("result".into(), "pass".into());
    obj.insert("missing".into(), json!([]));
    obj.remove("reason");
    obj.insert("id".into(), "gat_01JC0000000000000000000016".into());
    obj.insert(
        "checker".into(),
        json!({"kind": "hive", "check": "g6.rerun"}),
    );
    if let Some(ci) = ci {
        obj.insert("ci".into(), ci);
    }
    record
}

pub(crate) fn green_ci(commit: &Value) -> Value {
    json!({"provider": "gitlab", "project": 5, "repo": "leanctx/lean-ctx",
        "pipeline_id": "77", "url": "https://gitlab.example/p/77", "commit": commit,
        "status": "success", "required_jobs": ["test"],
        "jobs": [{"name": "test", "status": "success"}]})
}

fn green_record(repo: &Repo, spec: &str, head: &str) -> Value {
    let inputs = super::gate::inputs_for(
        &serde_json::from_value::<TaskProcessV1>(m_cargo_process(spec)).unwrap(),
        None,
    )
    .unwrap();
    let instance = serde_json::to_value(
        super::gate::g6_record(&repo.root, &dirs(repo), head, &inputs).unwrap(),
    )
    .unwrap();
    as_hive_record(&instance, Some(green_ci(&head.into())))
}

#[test]
fn verify_without_a_g6_record_is_refused() {
    let (repo, spec) = spec_repo();
    let head = repo.commit("work");
    let t = task(
        "submitted",
        this_instance(),
        Some(m_process(&spec, "true")),
        &head,
    );
    let text = refusal(verify_gate(&t, &repo.root, &work(&repo)).unwrap_err());
    assert!(text.starts_with("not supported by Hive yet"), "{text}");
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the G6 gate run is Unix-only (passwd home, rustup PATH)"
)]
fn verify_needs_a_green_record_of_the_judged_commit_with_locked_paths_intact() {
    let (repo, spec, head) = green_cargo_repo();
    let mut process = m_cargo_process(&spec);
    process["g6"] = green_record(&repo, &spec, &head);

    // The happy path: green, same commit, locked paths unchanged.
    let t = task("submitted", this_instance(), Some(process.clone()), &head);
    verify_gate(&t, &repo.root, &work(&repo)).unwrap();

    // A red record.
    let mut red = process.clone();
    red["g6"]["result"] = "fail".into();
    red["g6"]["reason"] = "the run exited 1".into();
    red["g6"]["rerun"]["features"][0]["passes"] = false.into();
    red["g6"]["rerun"]["exit_code"] = 1.into();
    let t = task("submitted", this_instance(), Some(red), &head);
    assert!(
        refusal(verify_gate(&t, &repo.root, &work(&repo)).unwrap_err()).contains("green G6 record")
    );

    // A record of another commit.
    repo.write("tests/refund.rs", "weakened after G6\n");
    let later = repo.commit("weaken");
    let t = task("submitted", this_instance(), Some(process.clone()), &later);
    assert!(refusal(verify_gate(&t, &repo.root, &work(&repo)).unwrap_err()).contains("judged"));

    // Locked paths changed in the judged commit: checked again before verify.
    let mut stale = process;
    stale["g6"]["artifact"]["ref"] = later.clone().into();
    stale["g6"]["ci"]["commit"] = later.clone().into();
    stale["g6"]["rerun"]["commit"] = later.clone().into();
    let t = task("submitted", this_instance(), Some(stale), &later);
    let text = refusal(verify_gate(&t, &repo.root, &work(&repo)).unwrap_err());
    assert!(text.contains("tests/refund.rs modified"), "{text}");
}

// --- review r1 F4: the checker is a live presence, bound to its caller --

/// An `--agent` no live bus presence of this Instance has is refused.
#[test]
fn an_invented_checker_is_refused() {
    let text = refusal(checker_identity("mcp-999-invented", None, false).unwrap_err());
    assert!(text.contains("no live bus agent"), "{text}");
    // The local registry knows no such agent either.
    assert!(local_presence("mcp-999-invented-by-a-test").is_none());
    // A presence for another id is not this one.
    let other = presence("mcp-200-reviewer", "code_reviewer", true);
    assert!(checker_identity("mcp-999-invented", Some(other), false).is_err());
}

/// A live presence the calling process neither registered nor shares an
/// app session with (`env -u LEAN_CTX_SESSION_ID lean-ctx tasks verify
/// --agent <someone else>`) is refused.
#[test]
fn a_checker_this_process_does_not_act_for_is_refused() {
    let elsewhere = presence("mcp-200-reviewer", "code_reviewer", false);
    let text = refusal(checker_identity("mcp-200-reviewer", Some(elsewhere), false).unwrap_err());
    assert!(text.contains("does not act for"), "{text}");
}

/// A fresh registration in the author's own session or process is no
/// other checker: the same app session or the same registering process.
#[test]
fn a_re_registration_in_the_authors_session_is_the_author() {
    let submitted = task("submitted", this_instance(), None, "9f01cf32a3");
    let author = SessionIdentity {
        agent: "mcp-100-worker".into(),
        session: None,
        process: Some("4242@99".into()),
    };
    let t = trace("mcp-100-worker", Some(author.clone()), None);
    // Same MCP process, new agent id.
    let mut again = presence("mcp-101-tester", "tester", true);
    again.process = Some("4242@99".into());
    let checker = checker_identity("mcp-101-tester", Some(again), true).unwrap();
    let text = refusal(checker_not_author(&submitted, &checker, Some(&t)).unwrap_err());
    assert!(text.starts_with("self_check"), "{text}");
    // Same app session, another process (an MCP restart).
    let author = SessionIdentity {
        session: Some("ses-worker".into()),
        ..author
    };
    let t = trace("mcp-100-worker", Some(author), None);
    let mut restarted = presence("mcp-102-tester", "tester", true);
    restarted.app_session = Some("ses-worker".into());
    let checker = checker_identity("mcp-102-tester", Some(restarted), true).unwrap();
    assert!(checker_not_author(&submitted, &checker, Some(&t)).is_err());
    // The claimant's session never checks either, by session not only id.
    let mut t = trace("mcp-100-worker", Some(session("mcp-300-other", None)), None);
    t.claimed_session = Some(SessionIdentity {
        agent: "mcp-100-worker".into(),
        session: None,
        process: Some("4242@99".into()),
    });
    let mut same_process = presence("mcp-103-tester", "tester", true);
    same_process.process = Some("4242@99".into());
    let checker = checker_identity("mcp-103-tester", Some(same_process), true).unwrap();
    assert!(checker_not_author(&submitted, &checker, Some(&t)).is_err());
    // Another session in another process verifies.
    let checker = checker_identity(
        "mcp-200-tester",
        Some(presence("mcp-200-tester", "tester", true)),
        true,
    )
    .unwrap();
    checker_not_author(&submitted, &checker, Some(&t)).unwrap();
}

/// A process task is verified in a verifying role only (§3.1).
#[test]
fn a_process_task_needs_a_verifying_role() {
    let developer = presence("mcp-200-dev", "developer", true);
    let text = refusal(checker_identity("mcp-200-dev", Some(developer.clone()), true).unwrap_err());
    assert!(text.starts_with("not_permitted"), "{text}");
    // A task that is not a process task keeps E48's rule: any other agent.
    checker_identity("mcp-200-dev", Some(developer), false).unwrap();
    for role in VERIFYING_ROLES {
        checker_identity("mcp-200-x", Some(presence("mcp-200-x", role, true)), true).unwrap();
    }
}

// --- review r1 F5: a Hive record passes only with green CI ----------------

#[test]
#[cfg_attr(
    windows,
    ignore = "the G6 gate run is Unix-only (passwd home, rustup PATH)"
)]
fn verify_refuses_a_hive_record_without_green_ci() {
    let (repo, spec, head) = green_cargo_repo();
    let green = green_record(&repo, &spec, &head);
    let with = |g6: Value| {
        let mut process = m_cargo_process(&spec);
        process["g6"] = g6;
        task("submitted", this_instance(), Some(process), &head)
    };
    verify_gate(&with(green.clone()), &repo.root, &work(&repo)).unwrap();

    // No ci: this Hive does not do §5.1 yet.
    let mut no_ci = green.clone();
    no_ci.as_object_mut().unwrap().remove("ci");
    let text = refusal(verify_gate(&with(no_ci), &repo.root, &work(&repo)).unwrap_err());
    assert!(text.starts_with("not supported by Hive yet"), "{text}");
    assert!(text.contains("ci"), "{text}");

    // A pipeline for the parent, or a failed required job.
    let mut parent = green.clone();
    parent["ci"]["commit"] = spec.clone().into();
    let text = refusal(verify_gate(&with(parent), &repo.root, &work(&repo)).unwrap_err());
    assert!(text.contains("ci_commit"), "{text}");
    let mut failed = green.clone();
    failed["ci"]["jobs"][0]["status"] = "failed".into();
    let text = refusal(verify_gate(&with(failed), &repo.root, &work(&repo)).unwrap_err());
    assert!(text.contains("ci_required_jobs"), "{text}");

    // The Instance's own record (blocked on ci_pipeline) is no pass.
    let mut instance = green;
    instance["result"] = "blocked".into();
    instance["missing"] = json!(["ci_pipeline"]);
    instance["reason"] = "awaiting ci".into();
    let text = refusal(verify_gate(&with(instance), &repo.root, &work(&repo)).unwrap_err());
    assert!(text.contains("green G6 record"), "{text}");
}

// --- review r2 F9: the CLI binds by process ancestry, never the env ------

/// A presence binds the calling process only when the session that
/// started its MCP server is an ancestor of the caller; an app session id
/// in the environment never binds.
#[cfg(target_os = "linux")]
#[test]
fn the_cli_binding_is_process_ancestry_never_the_env() {
    // Registered by a process this process started: its host is us.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    assert!(binds_caller(false, child.id()));
    let _ = child.kill();
    let _ = child.wait();
    // Registered by a process whose host is no ancestor of ours: a kernel
    // thread, started by kthreadd (pid 2).
    let foreign = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .find(|pid| *pid > 2 && super::parent_of(*pid) == Some(2));
    if let Some(foreign) = foreign {
        assert!(!binds_caller(false, foreign));
    }
    assert!(!binds_caller(false, 1));
    // The process that registered it is bound whatever its ancestry.
    assert!(binds_caller(true, 1));
}
