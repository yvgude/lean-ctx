use serde::Deserialize;

use crate::dashboard::routes::helpers::{
    detect_project_root_for_dashboard, json_err, json_ok, normalize_dashboard_demo_path,
};

pub(super) fn get_route(path: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/context-control" => Some(control()),
        "/api/context-handles" => Some(handles()),
        "/api/context-overlay-history" => Some(overlay_history()),
        "/api/context-plan" => Some(plan()),
        _ => None,
    }
}

pub(super) fn post_route(path: &str, body: &str) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/context-overlay" => Some(post_overlay(body)),
        "/api/context-policy" => Some(post_policy(body)),
        _ => None,
    }
}

fn control() -> (&'static str, &'static str, String) {
    let project_root = detect_project_root_for_dashboard();
    let mut ledger = crate::core::context_ledger::ContextLedger::load();
    let mut overlays = crate::core::context_overlay::OverlayStore::load_project(
        &std::path::PathBuf::from(&project_root),
    );
    let mut args = serde_json::Map::new();
    args.insert(
        "action".to_string(),
        serde_json::Value::String("list".to_string()),
    );
    // `list` only reads; a GET saves nothing (the writes are POST
    // `/api/context-overlay` and `/api/context-policy`).
    let result = crate::tools::ctx_control::handle(Some(&args), &mut ledger, &mut overlays);
    let payload = serde_json::json!({
        "result": result,
        "overlays": overlays.all(),
    });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn handles() -> (&'static str, &'static str, String) {
    let ledger = crate::core::context_ledger::ContextLedger::load();
    let project_root = detect_project_root_for_dashboard();
    let policies = crate::core::context_policies::PolicySet::load_project(
        &std::path::PathBuf::from(&project_root),
    );
    let candidates = crate::tools::ctx_plan::plan_to_candidates(&ledger, &policies);
    let mut registry = crate::core::context_handles::HandleRegistry::new();
    for c in &candidates {
        if c.state == crate::core::context_field::ContextState::Excluded {
            continue;
        }
        let summary = format!("{} {}", c.path, c.selected_view.as_str());
        registry.register(
            c.id.clone(),
            c.kind,
            &c.path,
            &summary,
            &c.view_costs,
            c.phi,
            c.pinned,
        );
    }
    let json = serde_json::to_string(&registry).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

fn overlay_history() -> (&'static str, &'static str, String) {
    let project_root = detect_project_root_for_dashboard();
    let store = crate::core::context_overlay::OverlayStore::load_project(
        &std::path::PathBuf::from(&project_root),
    );
    let json = serde_json::to_string(store.all()).unwrap_or_else(|_| "[]".to_string());
    ("200 OK", "application/json", json)
}

fn plan() -> (&'static str, &'static str, String) {
    let ledger = crate::core::context_ledger::ContextLedger::load();
    let project_root = detect_project_root_for_dashboard();
    let policies = crate::core::context_policies::PolicySet::load_project(
        &std::path::PathBuf::from(&project_root),
    );
    let text = crate::tools::ctx_plan::handle(None, &ledger, &policies);
    let payload = serde_json::json!({ "plan": text });
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", json)
}

#[derive(Deserialize)]
struct OverlayReq {
    action: String,
    path: String,
    #[serde(default)]
    value: Option<serde_json::Value>,
}

fn post_overlay(body: &str) -> (&'static str, &'static str, String) {
    let req: OverlayReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };
    let path_norm = normalize_dashboard_demo_path(req.path.trim());
    if path_norm.is_empty() {
        return (
            "400 Bad Request",
            "application/json",
            json_err("path is required"),
        );
    }
    let project_root = detect_project_root_for_dashboard();
    let root_path = std::path::PathBuf::from(&project_root);

    let mut ledger = crate::core::context_ledger::ContextLedger::load();
    let mut overlays = crate::core::context_overlay::OverlayStore::load_project(&root_path);

    let action = match req.action.as_str() {
        "priority" => "set_priority".to_string(),
        other => other.to_string(),
    };

    // #715: the dashboard Evict button used to write only an exclude overlay —
    // the ledger entry survived and pressure never dropped. A real eviction
    // removes the entry (resolving partial paths) AND excludes the resolved
    // canonical path against re-accumulation.
    if action == "evict" {
        let outcomes =
            ledger.evict_paths_resolved(&[path_norm.as_str()], Some(project_root.as_str()));
        let outcome = &outcomes[0];
        for resolved in outcomes.iter().filter_map(|o| o.resolved.as_deref()) {
            let item_id = crate::core::context_field::ContextItemId::from_file(resolved);
            overlays.add(crate::core::context_overlay::ContextOverlay::new(
                item_id,
                crate::core::context_overlay::OverlayOp::Exclude {
                    reason: "evicted via dashboard".into(),
                },
                crate::core::context_overlay::OverlayScope::Project,
                String::new(),
                crate::core::context_overlay::OverlayAuthor::User,
            ));
        }
        ledger.save();
        if let Err(e) = overlays.save_project(&root_path) {
            return (
                "500 Internal Server Error",
                "application/json",
                json_err(&e),
            );
        }
        if outcome.resolved.is_none() {
            let msg = if outcome.ambiguous.is_empty() {
                format!("'{path_norm}' not in ledger")
            } else {
                format!(
                    "'{path_norm}' is ambiguous ({}) — use a longer suffix",
                    outcome.ambiguous.join(", ")
                )
            };
            return ("400 Bad Request", "application/json", json_err(&msg));
        }
        let pressure = ledger.pressure();
        let payload = serde_json::json!({
            "ok": true,
            "evicted": outcome.resolved,
            "pressure": pressure.utilization,
        });
        let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
        return ("200 OK", "application/json", json);
    }

    if action == "compress" {
        let item_id = crate::core::context_field::ContextItemId::from_file(&path_norm);
        overlays.add(crate::core::context_overlay::ContextOverlay::new(
            item_id,
            crate::core::context_overlay::OverlayOp::SetView(
                crate::core::context_field::ViewKind::Map,
            ),
            crate::core::context_overlay::OverlayScope::Project,
            String::new(),
            crate::core::context_overlay::OverlayAuthor::User,
        ));
        ledger.save();
        if let Err(e) = overlays.save_project(&root_path) {
            return (
                "500 Internal Server Error",
                "application/json",
                json_err(&e),
            );
        }
        return ("200 OK", "application/json", json_ok());
    }

    if action == "expire" {
        let target = crate::core::context_field::ContextItemId::from_file(&path_norm);
        let secs: u64 = req
            .value
            .as_ref()
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0);
        let op = crate::core::context_overlay::OverlayOp::Expire { after_secs: secs };
        let mut store = crate::core::context_overlay::OverlayStore::load_project(&root_path);
        store.add(crate::core::context_overlay::ContextOverlay::new(
            target,
            op,
            crate::core::context_overlay::OverlayScope::Project,
            String::new(),
            crate::core::context_overlay::OverlayAuthor::User,
        ));
        if let Err(e) = store.save_project(&root_path) {
            return (
                "500 Internal Server Error",
                "application/json",
                json_err(&e),
            );
        }
        return ("200 OK", "application/json", json_ok());
    }

    let mut args = serde_json::Map::new();
    args.insert("action".into(), serde_json::Value::String(action));
    args.insert(
        "target".into(),
        serde_json::Value::String(path_norm.clone()),
    );
    args.insert("scope".into(), serde_json::Value::String("project".into()));
    if let Some(v) = &req.value {
        let val_str = match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => {
                if *b {
                    "verbatim".to_string()
                } else {
                    "false".to_string()
                }
            }
            other => other.to_string(),
        };
        args.insert("value".into(), serde_json::Value::String(val_str));
    }

    let _result = crate::tools::ctx_control::handle(Some(&args), &mut ledger, &mut overlays);
    ledger.save();
    if let Err(e) = overlays.save_project(&root_path) {
        return (
            "500 Internal Server Error",
            "application/json",
            json_err(&e),
        );
    }
    ("200 OK", "application/json", json_ok())
}

#[derive(Deserialize)]
struct PolicyReq {
    action: String,
    rule: serde_json::Value,
}

fn post_policy(body: &str) -> (&'static str, &'static str, String) {
    let req: PolicyReq = match serde_json::from_str(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                "400 Bad Request",
                "application/json",
                json_err(&format!("invalid JSON: {e}")),
            );
        }
    };
    let project_root = detect_project_root_for_dashboard();
    let root_path = std::path::PathBuf::from(&project_root);
    let mut policies = crate::core::context_policies::PolicySet::load_project(&root_path);

    match req.action.as_str() {
        "add" => {
            let rule: crate::core::context_policies::ContextPolicy =
                match serde_json::from_value(req.rule) {
                    Ok(p) => p,
                    Err(e) => {
                        return (
                            "400 Bad Request",
                            "application/json",
                            json_err(&format!("invalid rule: {e}")),
                        );
                    }
                };
            if rule.name.trim().is_empty() || rule.match_pattern.trim().is_empty() {
                return (
                    "400 Bad Request",
                    "application/json",
                    json_err("rule.name and rule.match_pattern are required"),
                );
            }
            policies.policies.push(rule);
            if let Err(e) = policies.save_project(&root_path) {
                return (
                    "500 Internal Server Error",
                    "application/json",
                    json_err(&e),
                );
            }
            ("200 OK", "application/json", json_ok())
        }
        "remove" => {
            let name = req
                .rule
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let Some(name) = name else {
                return (
                    "400 Bad Request",
                    "application/json",
                    json_err("remove requires rule.name"),
                );
            };
            let before = policies.policies.len();
            policies.policies.retain(|p| p.name != name);
            if policies.policies.len() == before {
                return (
                    "400 Bad Request",
                    "application/json",
                    json_err("no policy matched name"),
                );
            }
            if let Err(e) = policies.save_project(&root_path) {
                return (
                    "500 Internal Server Error",
                    "application/json",
                    json_err(&e),
                );
            }
            ("200 OK", "application/json", json_ok())
        }
        _ => (
            "400 Bad Request",
            "application/json",
            json_err("unknown action"),
        ),
    }
}
