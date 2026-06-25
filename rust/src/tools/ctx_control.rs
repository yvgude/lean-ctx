//! `ctx_control` -- Universal context manipulation tool.
//!
//! Single entry point for `include/exclude/pin/rewrite/set_view` operations.
//! Delegates to the Overlay and Ledger systems.

use serde_json::Value;

use crate::core::context_field::{ContextItemId, ContextState, ViewKind};
use crate::core::context_ledger::ContextLedger;
use crate::core::context_overlay::{
    ContextOverlay, OverlayAuthor, OverlayId, OverlayOp, OverlayScope, OverlayStore,
};

pub fn handle(
    args: Option<&serde_json::Map<String, Value>>,
    ledger: &mut ContextLedger,
    overlays: &mut OverlayStore,
) -> String {
    let action = get_str(args, "action").unwrap_or_default();
    let target = get_str(args, "target").unwrap_or_default();
    let value = get_str(args, "value");
    let scope_str = get_str(args, "scope").unwrap_or_else(|| "session".to_string());
    let reason = get_str(args, "reason").unwrap_or_else(|| action.clone());

    let scope = match scope_str.as_str() {
        "call" => OverlayScope::Call,
        "project" => OverlayScope::Project,
        "global" => OverlayScope::Global,
        _ => OverlayScope::Session,
    };

    let item_id = resolve_target(&target, ledger);

    match action.as_str() {
        "exclude" => {
            let op = OverlayOp::Exclude { reason: reason.clone() };
            apply_overlay(overlays, &item_id, op, scope);
            ledger.set_state(&target, ContextState::Excluded);
            format!("[ctx_control] excluded {target}: {reason}")
        }
        "include" => {
            let op = OverlayOp::Include;
            apply_overlay(overlays, &item_id, op, scope);
            ledger.set_state(&target, ContextState::Included);
            format!("[ctx_control] included {target}")
        }
        "pin" => {
            let verbatim = value.as_deref() == Some("verbatim");
            let op = OverlayOp::Pin { verbatim };
            apply_overlay(overlays, &item_id, op, scope);
            ledger.set_state(&target, ContextState::Pinned);
            format!("[ctx_control] pinned {target}")
        }
        "unpin" => {
            let op = OverlayOp::Unpin;
            apply_overlay(overlays, &item_id, op, scope);
            ledger.set_state(&target, ContextState::Included);
            format!("[ctx_control] unpinned {target}")
        }
        "set_view" => {
            let view_str = value.unwrap_or_else(|| "full".to_string());
            let view = ViewKind::parse(&view_str);
            let op = OverlayOp::SetView(view);
            apply_overlay(overlays, &item_id, op, scope);
            format!("[ctx_control] set view for {target} to {view_str}")
        }
        "set_priority" => {
            let priority: f64 = value
                .as_deref()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.5);
            let op = OverlayOp::SetPriority {
                set_priority: priority,
            };
            apply_overlay(overlays, &item_id, op, scope);
            ledger.update_phi(&target, priority);
            format!("[ctx_control] set priority for {target} to {priority:.2}")
        }
        "mark_outdated" => {
            let op = OverlayOp::MarkOutdated;
            apply_overlay(overlays, &item_id, op, scope);
            ledger.set_state(&target, ContextState::Stale);
            format!("[ctx_control] marked {target} as outdated")
        }
        "reset" => {
            overlays.remove_for_item(&item_id);
            ledger.set_state(&target, ContextState::Included);
            format!("[ctx_control] reset all overlays for {target}")
        }
        "list" => {
            let items = overlays.all();
            if items.is_empty() {
                "[ctx_control] no active overlays".to_string()
            } else {
                let mut out = format!("[ctx_control] {} active overlays:\n", items.len());
                for ov in items {
                    let stale_tag = if ov.stale { " [stale]" } else { "" };
                    out.push_str(&format!(
                        "  {} → {} ({}){}\n",
                        format_target(&ov.target),
                        format_operation(&ov.operation),
                        format_scope(&ov.scope),
                        stale_tag,
                    ));
                }
                out
            }
        }
        "history" => {
            let history = overlays.for_item(&item_id);
            if history.is_empty() {
                format!("[ctx_control] no overlay history for {target}")
            } else {
                let mut out = format!(
                    "[ctx_control] {} overlays for {target}:\n",
                    history.len()
                );
                for ov in history {
                    let age = format_age(&ov.created_at);
                    out.push_str(&format!(
                        "  {} ({}, {})\n",
                        format_operation(&ov.operation),
                        format_scope(&ov.scope),
                        age,
                    ));
                }
                out
            }
        }
        "help" => {
            "[ctx_control] available actions:\n\
             \x20 pin <target>        — keep file in full mode (immune to eviction)\n\
             \x20 unpin <target>      — remove pin, allow compression\n\
             \x20 exclude <target>    — restrict to signatures only\n\
             \x20 include <target>    — restore normal access\n\
             \x20 set_view <target>   — force a specific read mode (value: full|map|signatures|...)\n\
             \x20 set_priority <target> — set Phi priority (value: 0.0-1.0)\n\
             \x20 mark_outdated <target> — flag as stale, forces re-read\n\
             \x20 reset <target>      — clear all overlays for this file\n\
             \x20 list               — show all active overlays\n\
             \x20 history <target>   — show overlay history for a file"
                .to_string()
        }
        _ => {
            let suggestion = suggest_action(&action);
            let base = format!("[ctx_control] unknown action: \"{action}\".");
            if let Some(s) = suggestion {
                format!("{base} Did you mean \"{s}\"?\nUse action=\"help\" for all available actions.")
            } else {
                format!("{base} Use action=\"help\" for all available actions.")
            }
        }
    }
}

fn resolve_target(target: &str, ledger: &ContextLedger) -> ContextItemId {
    if target.starts_with("file:")
        || target.starts_with("shell:")
        || target.starts_with("knowledge:")
    {
        ContextItemId(target.to_string())
    } else if let Some(stripped) = target.strip_prefix('@') {
        ContextItemId::from_file(stripped)
    } else if let Some(entry) = ledger.entries.iter().find(|e| e.path == target) {
        entry
            .id
            .clone()
            .unwrap_or_else(|| ContextItemId::from_file(target))
    } else {
        ContextItemId::from_file(target)
    }
}

fn apply_overlay(
    overlays: &mut OverlayStore,
    item_id: &ContextItemId,
    operation: OverlayOp,
    scope: OverlayScope,
) {
    let overlay = ContextOverlay {
        id: OverlayId::generate(item_id),
        target: item_id.clone(),
        operation,
        scope,
        before_hash: String::new(),
        author: OverlayAuthor::Agent("mcp".to_string()),
        created_at: chrono::Utc::now(),
        stale: false,
    };
    overlays.add(overlay);
}

const VALID_ACTIONS: &[&str] = &[
    "exclude",
    "include",
    "pin",
    "unpin",
    "set_view",
    "set_priority",
    "mark_outdated",
    "reset",
    "list",
    "history",
    "help",
];

fn suggest_action(input: &str) -> Option<&'static str> {
    let input_lower = input.to_lowercase();
    match input_lower.as_str() {
        "evict" | "remove" => return Some("exclude"),
        "compress" | "shrink" => return Some("set_view"),
        "budget" => return Some("set_priority"),
        _ => {}
    }
    VALID_ACTIONS
        .iter()
        .filter_map(|&action| {
            let dist = levenshtein(&input_lower, action);
            (dist <= 3).then_some((action, dist))
        })
        .min_by_key(|(_, dist)| *dist)
        .map(|(a, _)| a)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (rows, cols) = (a.len() + 1, b.len() + 1);
    let mut matrix = vec![vec![0usize; cols]; rows];
    for (i, row) in matrix.iter_mut().enumerate() {
        row[0] = i;
    }
    #[allow(clippy::needless_range_loop)]
    for j in 0..cols {
        matrix[0][j] = j;
    }
    for i in 1..rows {
        for j in 1..cols {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }
    matrix[a.len()][b.len()]
}

fn format_target(id: &ContextItemId) -> String {
    let s = id.0.as_str();
    if let Some(path) = s.strip_prefix("file:") {
        crate::core::protocol::shorten_path(path)
    } else {
        s.to_string()
    }
}

fn format_operation(op: &OverlayOp) -> String {
    match op {
        OverlayOp::Include => "included".to_string(),
        OverlayOp::Exclude { reason } if reason == "exclude" => "excluded".to_string(),
        OverlayOp::Exclude { reason } => format!("excluded ({reason})"),
        OverlayOp::Pin { verbatim: true } => "pinned (verbatim)".to_string(),
        OverlayOp::Pin { verbatim: false } => "pinned".to_string(),
        OverlayOp::Unpin => "unpinned".to_string(),
        OverlayOp::Rewrite { .. } => "rewrite".to_string(),
        OverlayOp::SetView(v) => format!("view: {}", v.as_str()),
        OverlayOp::SetPriority { set_priority } => format!("priority: {set_priority:.2}"),
        OverlayOp::MarkOutdated => "outdated".to_string(),
        OverlayOp::Expire { after_secs } => format!("expires in {after_secs}s"),
    }
}

fn format_scope(scope: &OverlayScope) -> &'static str {
    match scope {
        OverlayScope::Call => "this call",
        OverlayScope::Session => "this session",
        OverlayScope::Project => "persistent",
        OverlayScope::Global => "global",
        OverlayScope::Agent(_) => "agent",
    }
}

fn format_age(created_at: &chrono::DateTime<chrono::Utc>) -> String {
    let elapsed = chrono::Utc::now().signed_duration_since(*created_at);
    if elapsed.num_seconds() < 60 {
        "just now".to_string()
    } else if elapsed.num_minutes() < 60 {
        format!("{}m ago", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h ago", elapsed.num_hours())
    } else {
        format!("{}d ago", elapsed.num_days())
    }
}

fn get_str(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    args?
        .get(key)?
        .as_str()
        .map(std::string::ToString::to_string)
}
