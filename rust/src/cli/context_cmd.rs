// SPDX-License-Identifier: Apache-2.0

//! CLI subcommands for Context Field Theory tools:
//! `lean-ctx control`, `lean-ctx plan`, `lean-ctx compile`.
//!
//! `plan` and `compile` render locally from the persisted ledger: their MCP
//! tools were removed in 3.12.0, so the daemon cannot serve them.

use crate::core::context_ledger::ContextLedger;
use crate::core::context_overlay::OverlayStore;
use crate::core::context_policies::PolicySet;

pub(crate) fn cmd_control(args: &[String]) {
    if args.is_empty() {
        eprintln!(
            "Usage: lean-ctx control <action> [target] [--scope session|project|call] \
             [--reason \"...\"] [--value \"...\"]"
        );
        eprintln!(
            "Actions: exclude, include, pin, unpin, set_view, set_priority, mark_outdated, reset, list, history"
        );
        std::process::exit(1);
    }

    let action = &args[0];
    let target = args.get(1).map_or("", String::as_str);
    let scope = flag_value(args, "--scope").unwrap_or_else(|| "session".to_string());
    let reason = flag_value(args, "--reason");
    let value = flag_value(args, "--value");

    let mut json = serde_json::json!({
        "action": action,
        "target": target,
        "scope": scope,
    });
    if let Some(r) = &reason {
        json["reason"] = serde_json::Value::String(r.clone());
    }
    if let Some(v) = &value {
        json["value"] = serde_json::Value::String(v.clone());
    }

    #[cfg(unix)]
    {
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_control",
            Some(json.clone()),
        ) {
            println!("{out}");
            return;
        }
    }
    super::common::daemon_fallback_hint();

    let mut ledger = ContextLedger::load();
    let project_root = match super::common::require_project_root() {
        Ok(root) => std::path::PathBuf::from(root),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let mut overlays = OverlayStore::load_project(&project_root);

    let args_map = build_map(&[
        ("action", Some(action.clone())),
        ("target", Some(target.to_string())),
        ("scope", Some(scope)),
        ("reason", reason),
        ("value", value),
    ]);
    let root = project_root.to_string_lossy().into_owned();
    let result = super::common::CliToolRun::new("ctx_control", &root, Some(&args_map))
        .deliver_text(|| {
            crate::tools::ctx_control::handle(Some(&args_map), &mut ledger, &mut overlays)
        });
    ledger.save();
    let _ = overlays.save_project(&project_root);

    super::common::print_released(&result);
}

pub(crate) fn cmd_plan(args: &[String]) {
    let task = if args.is_empty() || args[0].starts_with('-') {
        "general".to_string()
    } else {
        args[0].clone()
    };
    let budget = budget_value(args);

    let mut json = serde_json::json!({ "task": task });
    if let Some(budget) = budget {
        json["budget"] = serde_json::json!(budget);
    }

    // The MCP tool `ctx_plan` was removed in 3.12.0; the daemon only answers
    // "was removed". The plan reads the persisted ledger, so it runs locally.
    let ledger = ContextLedger::load();
    let policies = PolicySet::defaults();

    let root = super::common::detect_project_root(args);
    let result = super::common::CliToolRun::new("ctx_plan", &root, json.as_object())
        .deliver_text(|| crate::tools::ctx_plan::handle(json.as_object(), &ledger, &policies));

    super::common::print_released(&result);
}

pub(crate) fn cmd_compile(args: &[String]) {
    let mode = flag_value(args, "--mode").unwrap_or_else(|| "handles".to_string());
    let budget = budget_value(args);

    let mut json = serde_json::json!({ "mode": mode });
    if let Some(budget) = budget {
        json["budget"] = serde_json::json!(budget);
    }

    // `ctx_compile` is no MCP tool since 3.12.0 (see `cmd_plan`): run locally.
    let ledger = ContextLedger::load();
    let policies = PolicySet::defaults();

    let root = super::common::detect_project_root(args);
    let result = super::common::CliToolRun::new("ctx_compile", &root, json.as_object())
        .deliver_text(|| crate::tools::ctx_compile::handle(json.as_object(), &ledger, &policies));

    super::common::print_released(&result);
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// A supplied hard budget must never silently fall back to the default.
fn budget_value(args: &[String]) -> Option<usize> {
    let mut budget = None;
    for (index, argument) in args.iter().enumerate() {
        let value = if argument == "--budget" {
            args.get(index + 1).map(String::as_str)
        } else if let Some(value) = argument.strip_prefix("--budget=") {
            Some(value)
        } else {
            continue;
        };
        let parsed = value
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<usize>().ok());
        if budget.is_some() || parsed.is_none() {
            eprintln!(
                "invalid_budget: supply --budget once with a nonnegative integer that fits this platform"
            );
            std::process::exit(2);
        }
        budget = parsed;
    }
    budget
}

fn build_map(pairs: &[(&str, Option<String>)]) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for (key, val) in pairs {
        if let Some(v) = val {
            map.insert((*key).to_string(), serde_json::Value::String(v.clone()));
        }
    }
    map
}

#[cfg(test)]
mod tests {
    /// F9a: a CLI command that asks the daemon for an MCP tool removed from
    /// the public surface only ever gets "was removed" back. Every daemon
    /// tool name in `cli/` must still be a live tool.
    #[test]
    fn cli_never_routes_a_removed_tool_through_the_daemon() {
        // Split so this literal is not itself a match.
        let marker = concat!("try_daemon_tool_call_", "blocking_text(");
        let cli = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("cli");
        let mut stack = vec![cli];
        let mut checked = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (index, _) in text.match_indices(marker) {
                    let rest = text[index + marker.len()..].trim_start();
                    let Some(rest) = rest.strip_prefix('"') else {
                        continue;
                    };
                    let name = &rest[..rest.find('"').unwrap()];
                    checked += 1;
                    assert!(
                        crate::server::dynamic_tools::removed_tool(name).is_none(),
                        "{} asks the daemon for the removed tool {name}",
                        path.display()
                    );
                }
            }
        }
        assert!(checked > 0, "the scan found no daemon tool calls");
    }
}
