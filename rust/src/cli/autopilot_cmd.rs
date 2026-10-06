// SPDX-License-Identifier: Apache-2.0

//! Personal, local inspection of receipt-validated adaptive learning.

use anyhow::{Result, bail, ensure};
use lean_ctx_protocol::{ProjectId, TenantId};
use serde_json::{Value, json};

use crate::core::context_kernel::autopilot::learning_store::{
    AdaptiveLearningStore, MAX_HISTORY, MAX_ROUTING_HISTORY,
};

const HELP: &str = "Usage: lean-ctx autopilot <status|export|history|explain|evidence|policy|reset> [--project-id ID] [--tenant-id ID]\n\
history: [--limit 1..1000]; explain: [--receipt-id ID]; evidence: [--learn];\n\
policy: [status|promote|monitor|rollback]; reset: --yes\n\
Default project ID is the current directory; use the exact TaskEnvelope project ID for another scope.\n\
Output is JSON. History/explain cover committed learning outcomes, not all executions.\n\
evidence: content-free read-strategy evidence per workload; --learn asks the optional\n\
licensed runtime for a candidate recommendation. Candidates are shown, never applied.\n\
policy: the scope's promoted read-strategy policy. promote/monitor keep the runtime's\n\
verdict on this scope's evidence (monitoring starts with a second promotion); rollback\n\
restores the last stable policy, or none. Planning\n\
records it in shadow unless intelligence_runtime.context_policy_apply is set.\n\
Reset removes this scope's learning, replay memory and history; backups are not erased.\n\
This command does not enable Pro or establish organization authorization.";

struct Options {
    action: String,
    project: ProjectId,
    tenant: Option<TenantId>,
    receipt: Option<String>,
    limit: usize,
    learn: bool,
    policy_action: String,
}

fn parse(rest: &[String]) -> Result<Options> {
    let action = rest.first().map_or("status", String::as_str);
    ensure!(
        matches!(
            action,
            "status" | "export" | "history" | "explain" | "evidence" | "policy" | "reset"
        ),
        "{HELP}"
    );
    let mut flags = &rest[rest.len().min(1)..];
    let mut policy_action = "status";
    if action == "policy"
        && let Some(sub) = flags.first().filter(|sub| !sub.starts_with("--"))
    {
        ensure!(
            matches!(sub.as_str(), "status" | "promote" | "monitor" | "rollback"),
            "unknown policy action: {sub}"
        );
        policy_action = sub;
        flags = &flags[1..];
    }
    let mut project = None;
    let mut tenant = None;
    let mut receipt = None;
    let mut limit = None;
    let mut confirmed = false;
    let mut learn = false;
    let mut args = flags.iter();
    while let Some(flag) = args.next() {
        if flag == "--yes" {
            ensure!(
                !confirmed && action == "reset",
                "--yes is only valid once for reset"
            );
            confirmed = true;
            continue;
        }
        if flag == "--learn" {
            ensure!(
                !learn && action == "evidence",
                "--learn is only valid once for evidence"
            );
            learn = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))?;
        match flag.as_str() {
            "--project-id" if project.is_none() => project = Some(ProjectId::new(value.clone())?),
            "--tenant-id" if tenant.is_none() => tenant = Some(TenantId::new(value.clone())?),
            "--receipt-id" if receipt.is_none() && action == "explain" => {
                lean_ctx_protocol::ReceiptId::new(value.clone())?;
                receipt = Some(value.clone());
            }
            "--limit" if limit.is_none() && action == "history" => {
                let count = value.parse::<usize>()?;
                ensure!(
                    (1..=MAX_HISTORY).contains(&count),
                    "history limit must be 1..={MAX_HISTORY}"
                );
                limit = Some(count);
            }
            _ => bail!("unknown, repeated or inapplicable option: {flag}"),
        }
    }
    ensure!(
        action != "reset" || confirmed,
        "reset requires --yes; it also clears receipt replay protection"
    );
    let project = match project {
        Some(project) => project,
        None => ProjectId::new(
            std::env::current_dir()?
                .to_str()
                .ok_or_else(|| {
                    anyhow::anyhow!("project directory must be UTF-8; specify --project-id")
                })?
                .to_owned(),
        )?,
    };
    Ok(Options {
        action: action.to_owned(),
        project,
        tenant,
        receipt,
        limit: limit.unwrap_or(20),
        learn,
        policy_action: policy_action.to_owned(),
    })
}

pub(super) fn cmd_autopilot(rest: &[String]) -> i32 {
    if rest
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        println!("{HELP}");
        return 0;
    }
    match execute(rest) {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(error) => {
            eprintln!("Autopilot: {error}");
            1
        }
    }
}

fn execute(rest: &[String]) -> Result<String> {
    // Parse fully before opening storage, especially for reset and unknown flags.
    let options = parse(rest)?;
    let mut store =
        AdaptiveLearningStore::open_default(options.project.clone(), options.tenant.clone())?;
    let output = render(&options, &mut store)?;
    Ok(serde_json::to_string_pretty(&output)?)
}

fn render(options: &Options, store: &mut AdaptiveLearningStore) -> Result<Value> {
    let mut output = json!({
        "schema_version": 1,
        "project_id": options.project,
        "tenant_id": options.tenant,
        "kind": "validated_learning_outcomes"
    });
    match options.action.as_str() {
        "status" => {
            let (learning, history) = store.inspect()?;
            let mut state = serde_json::to_value(learning)?;
            let receipts = state
                .as_object_mut()
                .expect("typed state object")
                .remove("processed_receipts")
                .expect("typed receipt set");
            state["processed_receipt_count"] =
                json!(receipts.as_array().expect("typed receipt array").len());
            output["learning"] = state;
            output["retained_history_count"] = json!(history.len());
        }
        "export" => {
            let (learning, history) = store.inspect()?;
            output["learning"] = serde_json::to_value(learning)?;
            output["history"] = serde_json::to_value(history)?;
            output["routing_history"] =
                serde_json::to_value(store.routing_history(MAX_ROUTING_HISTORY)?)?;
        }
        "history" => {
            output["history"] = serde_json::to_value(store.history(options.limit)?)?;
            output["routing_history"] =
                serde_json::to_value(store.routing_history(options.limit)?)?;
        }
        "explain" => {
            let entry = match &options.receipt {
                Some(receipt) => store.explain(receipt)?,
                None => store.history(1)?.into_iter().next(),
            };
            output["decision"] = serde_json::to_value(entry)?;
        }
        "evidence" => {
            let evidence = store.policy_evidence()?;
            // Shadow view: the planner's current learned mode next to what a
            // runtime would recommend. Nothing here changes planning.
            output["learned_mode"] = serde_json::to_value(store.load()?)?["learned_mode"].take();
            output["runtime"] = if options.learn {
                learn_view(&evidence)
            } else {
                json!({"requested": false})
            };
            output["evidence"] = serde_json::to_value(evidence)?;
        }
        "policy" => {
            output["policy"] = policy_view(options, store)?;
        }
        "reset" => {
            store.reset()?;
            output["reset"] = json!(true);
            output["replay_memory_cleared"] = json!(true);
        }
        _ => bail!("unsupported autopilot action"),
    }
    Ok(output)
}

fn learn_view(
    evidence: &lean_ctx_protocol::context_policy_evidence::ContextPolicyEvidenceV1,
) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    match crate::core::intelligence_runtime::context_policy::learn(evidence, now) {
        None => json!({"requested": true, "state": "not_configured"}),
        Some(Err(error)) => {
            json!({"requested": true, "state": "unavailable", "reason": error.to_string()})
        }
        Some(Ok(output)) => json!({
            "requested": true,
            "state": "answered",
            "applied": false,
            "result": output
        }),
    }
}

/// The scope's promoted read-strategy policy. Promotion and monitoring ask
/// the optional runtime for its verdict on this scope's evidence; this
/// command only keeps the result.
fn policy_view(options: &Options, store: &AdaptiveLearningStore) -> Result<Value> {
    use crate::core::context_store::policy_store::{self, PolicyEntry};
    use crate::core::intelligence_runtime::context_policy::{self, Operation};

    let scope = crate::core::context_store::task_scope(options.tenant.as_ref(), &options.project);
    let mut policies = policy_store::load(&scope);
    let apply = crate::core::config::Config::load_global()
        .intelligence_runtime
        .context_policy_apply;
    let mut view = json!({"mode": if apply { "apply" } else { "shadow" }});
    match options.policy_action.as_str() {
        "status" => {}
        "rollback" => {
            ensure!(policies.active.is_some(), "no active policy to roll back");
            // Without a stable predecessor, the stable state is no policy:
            // planning returns to the Engine's own modes.
            policies = policy_store::update(&scope, &policies, |stored| {
                match stored.last_stable.clone() {
                    Some(restored) => stored.roll_back_to(restored),
                    None => stored.active = None,
                }
            })
            .map_err(anyhow::Error::msg)?;
            view["rolled_back"] = json!(true);
        }
        action @ ("promote" | "monitor") => {
            let operation = if action == "promote" {
                Operation::Promote
            } else {
                ensure!(
                    policies.active.is_some() && policies.last_stable.is_some(),
                    "monitoring needs an active and a last stable policy"
                );
                Operation::Monitor
            };
            let evidence = store.policy_evidence()?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs());
            let answer = match context_policy::run(
                operation,
                &evidence,
                now,
                policies.active.as_ref().map(|entry| &entry.artifact),
                policies.last_stable.as_ref().map(|entry| &entry.artifact),
            ) {
                None => bail!("no licensed runtime is configured"),
                Some(Err(error)) => bail!("runtime unavailable: {error}"),
                Some(Ok(answer)) => answer,
            };
            let verdict = answer.policy.clone().map(|entry| PolicyEntry {
                policy: entry.policy,
                artifact: entry.artifact,
            });
            // The runtime's verdict must fit the state it judged: a promotion
            // succeeds the active policy, a rollback restores the stable one.
            // It is then kept against that state: a concurrent change fails
            // this command instead of being overwritten.
            let active = policies.active.as_ref().map(|entry| &entry.policy);
            let stable = policies.last_stable.as_ref().map(|entry| &entry.policy);
            match (answer.status.as_str(), &verdict) {
                ("promoted", Some(entry)) => ensure!(
                    active.is_none_or(|active| entry.policy.version > active.version)
                        && entry.policy.parent_version == active.map(|active| active.version),
                    "the runtime promoted a policy that does not succeed the active one"
                ),
                ("rolled_back", Some(entry)) => ensure!(
                    Some(&entry.policy) == stable,
                    "the runtime restored a policy other than the last stable one"
                ),
                _ => {}
            }
            match (answer.status.as_str(), verdict) {
                ("promoted", Some(entry)) => {
                    policies = policy_store::update(&scope, &policies, |stored| {
                        stored.promote(entry);
                    })
                    .map_err(anyhow::Error::msg)?;
                }
                ("rolled_back", Some(entry)) => {
                    policies = policy_store::update(&scope, &policies, |stored| {
                        stored.roll_back_to(entry);
                    })
                    .map_err(anyhow::Error::msg)?;
                }
                _ => {}
            }
            view["runtime"] = serde_json::to_value(&answer)?;
        }
        other => bail!("unknown policy action: {other}"),
    }
    view["active"] = serde_json::to_value(policies.active.as_ref().map(|entry| &entry.policy))?;
    view["last_stable"] =
        serde_json::to_value(policies.last_stable.as_ref().map(|entry| &entry.policy))?;
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn strict_options_and_reset_confirmation() {
        for invalid in [
            vec!["reset"],
            vec!["status", "--yes"],
            vec!["history", "--limit", "0"],
            vec!["history", "--limit", "1001"],
            vec!["status", "--receipt-id", "id"],
            vec!["status", "--project-id"],
            vec!["status", "--project-id", "a", "--project-id", "b"],
            vec!["export", "unexpected-file"],
            vec!["unknown"],
            vec!["status", "--learn"],
            vec!["evidence", "--learn", "--learn"],
            vec!["policy", "apply"],
            vec!["status", "promote"],
        ] {
            assert!(parse(&args(&invalid)).is_err(), "{invalid:?}");
        }
        assert!(
            parse(&args(&["evidence", "--learn", "--project-id", "p"]))
                .expect("valid evidence options")
                .learn
        );
        let policy = parse(&args(&["policy", "rollback", "--project-id", "p"]))
            .expect("valid policy options");
        assert_eq!(
            (policy.action.as_str(), policy.policy_action.as_str()),
            ("policy", "rollback")
        );
        assert_eq!(
            parse(&args(&["policy", "--project-id", "p"]))
                .expect("policy defaults to status")
                .policy_action,
            "status"
        );
        let options = parse(&args(&[
            "reset",
            "--yes",
            "--project-id",
            "exact-project",
            "--tenant-id",
            "tenant",
        ]))
        .unwrap();
        assert_eq!(options.project.as_str(), "exact-project");
        assert_eq!(options.tenant.unwrap().as_str(), "tenant");
    }

    #[test]
    fn empty_store_inspection_is_truthful_and_deterministic() {
        let directory = tempfile::tempdir().unwrap();
        let project = ProjectId::new("cli-project").unwrap();
        let mut store = AdaptiveLearningStore::new(
            rusqlite::Connection::open(directory.path().join("test.db")).unwrap(),
            project,
            None,
        )
        .unwrap();
        for action in ["status", "export", "history", "explain", "evidence"] {
            let options = parse(&args(&[action, "--project-id", "cli-project"])).unwrap();
            let first = render(&options, &mut store).unwrap();
            assert_eq!(render(&options, &mut store).unwrap(), first);
            assert_eq!(first["kind"], "validated_learning_outcomes");
            if action == "explain" {
                assert!(first["decision"].is_null());
            }
            if action == "history" {
                assert_eq!(first["history"], json!([]));
            }
            if action == "status" {
                assert_eq!(first["learning"]["processed_receipt_count"], 0);
                assert!(first["learning"].get("processed_receipts").is_none());
            }
            if action == "evidence" {
                assert_eq!(first["evidence"]["records"], json!([]));
                assert_eq!(first["runtime"], json!({"requested": false}));
                assert!(first["learned_mode"].is_null());
            }
        }
    }
}
