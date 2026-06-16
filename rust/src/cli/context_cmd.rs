//! CLI subcommands for Context Field Theory tools:
//! `lean-ctx control`, `lean-ctx plan`, `lean-ctx compile`.

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
    let project_root = std::env::current_dir().unwrap_or_default();
    let mut overlays = OverlayStore::load_project(&project_root);

    let args_map = build_map(&[
        ("action", Some(action.clone())),
        ("target", Some(target.to_string())),
        ("scope", Some(scope)),
        ("reason", reason),
        ("value", value),
    ]);
    let result = crate::tools::ctx_control::handle(Some(&args_map), &mut ledger, &mut overlays);
    ledger.save();
    let _ = overlays.save_project(&project_root);

    println!("{result}");
}

pub(crate) fn cmd_plan(args: &[String]) {
    let task = if args.is_empty() || args[0].starts_with('-') {
        "general".to_string()
    } else {
        args[0].clone()
    };
    let budget = flag_value(args, "--budget");

    let mut json = serde_json::json!({ "task": task });
    if let Some(b) = &budget
        && let Ok(n) = b.parse::<u64>()
    {
        json["budget"] = serde_json::Value::Number(n.into());
    }

    #[cfg(unix)]
    {
        if let Some(out) =
            crate::daemon_client::try_daemon_tool_call_blocking_text("ctx_plan", Some(json.clone()))
        {
            println!("{out}");
            return;
        }
    }
    super::common::daemon_fallback_hint();

    let ledger = ContextLedger::load();
    let policies = PolicySet::defaults();

    let args_map = build_map(&[("task", Some(task)), ("budget", budget)]);
    let result = crate::tools::ctx_plan::handle(Some(&args_map), &ledger, &policies);

    println!("{result}");
}

pub(crate) fn cmd_compile(args: &[String]) {
    let mode = flag_value(args, "--mode").unwrap_or_else(|| "handles".to_string());
    let budget = flag_value(args, "--budget");

    let mut json = serde_json::json!({ "mode": mode });
    if let Some(b) = &budget
        && let Ok(n) = b.parse::<u64>()
    {
        json["budget"] = serde_json::Value::Number(n.into());
    }

    #[cfg(unix)]
    {
        if let Some(out) = crate::daemon_client::try_daemon_tool_call_blocking_text(
            "ctx_compile",
            Some(json.clone()),
        ) {
            println!("{out}");
            return;
        }
    }
    super::common::daemon_fallback_hint();

    let ledger = ContextLedger::load();
    let policies = PolicySet::defaults();

    let args_map = build_map(&[("mode", Some(mode)), ("budget", budget)]);
    let result = crate::tools::ctx_compile::handle(Some(&args_map), &ledger, &policies);

    println!("{result}");
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
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
