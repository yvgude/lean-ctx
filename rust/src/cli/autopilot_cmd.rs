// SPDX-License-Identifier: Apache-2.0

//! Personal, local inspection of receipt-validated adaptive learning.

use anyhow::{Result, bail, ensure};
use lean_ctx_protocol::{ProjectId, TenantId};
use serde_json::{Value, json};

use crate::core::context_kernel::autopilot::learning_store::{
    AdaptiveLearningStore, MAX_HISTORY, MAX_ROUTING_HISTORY,
};

const HELP: &str = "Usage: lean-ctx autopilot <status|export|history|explain|evidence|reset> [--project-id ID] [--tenant-id ID]\n\
history: [--limit 1..1000]; explain: [--receipt-id ID]; reset: --yes\n\
Default project ID is the current directory; use the exact TaskEnvelope project ID for another scope.\n\
Output is JSON. History/explain cover committed learning outcomes, not all executions.\n\
evidence: content-free read-strategy evidence per workload, next to the planner's learned mode.\n\
Deprecated, no effect: `policy [status|promote|monitor|rollback]` and `evidence --learn`.\n\
Reset removes this scope's learning, replay memory and history; backups are not erased.\n\
This command does not establish organization authorization.";

/// `autopilot policy` and `evidence --learn` asked the licensed runtime for a
/// learned read strategy, which the engine no longer uses. They stay accepted
/// until their announced removal (DEPRECATIONS.toml) and report `retired`.
const RETIRED_NOTE: &str = "Learned read-strategy policies are retired; read strategy is chosen locally. \
This option has no effect and will be removed (see `lean-ctx doctor`).";

struct Options {
    action: String,
    project: ProjectId,
    tenant: Option<TenantId>,
    receipt: Option<String>,
    limit: usize,
    learn: bool,
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
    if action == "policy"
        && let Some(sub) = flags.first().filter(|sub| !sub.starts_with("--"))
    {
        ensure!(
            matches!(sub.as_str(), "status" | "promote" | "monitor" | "rollback"),
            "unknown policy action: {sub}"
        );
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
        Ok(output) => match released(&super::common::detect_project_root(&[]), output) {
            Ok(document) => {
                super::common::print_released(&document);
                0
            }
            Err(reason) => super::common::exit_withheld(&reason),
        },
        Err(error) => {
            eprintln!("Autopilot: {error}");
            1
        }
    }
}

/// The JSON document passes the release point like every CLI output; a
/// masked value stands in its place, so the document stays valid.
fn released(project_root: &str, document: String) -> Result<String, String> {
    super::common::CliToolRun::new("ctx_metrics", project_root, None).deliver_json(|| document)
}

fn execute(rest: &[String]) -> Result<String> {
    // Parse fully before opening storage, especially for reset and unknown flags.
    let options = parse(rest)?;
    if options.action == "policy" || options.learn {
        eprintln!("Autopilot: {RETIRED_NOTE}");
    }
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
            // The planner's current learned mode next to its evidence.
            output["learned_mode"] = serde_json::to_value(store.load()?)?["learned_mode"].take();
            output["runtime"] = json!({"requested": options.learn, "status": "retired"});
            output["evidence"] = serde_json::to_value(evidence)?;
        }
        "policy" => {
            output["policy"] = json!({"status": "retired", "note": RETIRED_NOTE});
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
            vec!["policy", "bogus"],
            vec!["status", "--learn"],
        ] {
            assert!(parse(&args(&invalid)).is_err(), "{invalid:?}");
        }
        // Deprecated surfaces keep parsing until their announced removal.
        for deprecated in [
            vec!["policy"],
            vec!["policy", "promote"],
            vec!["evidence", "--learn"],
        ] {
            assert!(parse(&args(&deprecated)).is_ok(), "{deprecated:?}");
        }
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

    /// The document was printed directly; it now passes the release point
    /// and stays valid JSON when a value is masked.
    #[test]
    fn the_document_is_released() {
        use crate::cli::common::carrier_tests_support::{AWS_KEY, governed, project};
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        governed();
        let (_project, root) = project();
        let clean = serde_json::to_string_pretty(&json!({"schema_version": 1})).unwrap();
        assert_eq!(released(&root, clean.clone()), Ok(clean));
        let keyed = json!({"project_id": format!("p {AWS_KEY}")}).to_string();
        let document = released(&root, keyed).expect("masked, not withheld");
        assert!(!document.contains(AWS_KEY), "{document}");
        assert!(
            serde_json::from_str::<Value>(&document).is_ok(),
            "{document}"
        );
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
                assert!(first["learned_mode"].is_null());
            }
        }
    }
}
