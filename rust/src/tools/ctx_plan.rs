//! `ctx_plan` -- Context planning tool.
//!
//! Given a task and budget, computes the optimal context plan using the
//! Context Field potential function, intent router, and deficit analysis.

use serde_json::Value;

use crate::core::context_compiler::CompileCandidate;
use crate::core::context_field::{
    ContextField, ContextItemId, ContextKind, ContextState, FieldSignals, TokenBudget, ViewCosts,
    ViewKind,
};
use crate::core::context_ledger::ContextLedger;
use crate::core::context_policies::PolicySet;

const FALLBACK_BUDGET: usize = 12_000;

#[must_use]
pub fn handle(
    args: Option<&serde_json::Map<String, Value>>,
    ledger: &ContextLedger,
    policies: &PolicySet,
) -> String {
    let task = get_str(args, "task").unwrap_or_else(|| "general".to_string());
    let explicit_budget = args
        .and_then(|a| a.get("budget"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .map(|b| b as usize);
    let default_budget = if ledger.window_size > 0 {
        ledger.window_size
    } else {
        FALLBACK_BUDGET
    };
    let budget_tokens = explicit_budget.unwrap_or(default_budget);
    let profile = get_str(args, "profile").unwrap_or_else(|| "balanced".to_string());

    let field = ContextField::new();
    let budget = TokenBudget {
        total: budget_tokens,
        used: 0,
    };
    let temperature = budget.temperature();

    let intent_route = crate::core::intent_router::route_v1(&task);
    let intent_mode = &intent_route.decision.effective_read_mode;

    let mut plan_items: Vec<PlanItem> = Vec::new();
    let mut total_estimated = 0usize;

    for entry in &ledger.entries {
        let path = &entry.path;
        let seen_before = true;

        let effective_state = policies.effective_state(
            path,
            entry.state.unwrap_or(ContextState::Included),
            seen_before,
            entry.original_tokens,
        );
        if effective_state == ContextState::Excluded {
            plan_items.push(PlanItem {
                path: path.clone(),
                recommended_view: "excluded".to_string(),
                estimated_tokens: 0,
                phi: 0.0,
                state: "excluded".to_string(),
                reason: "policy".to_string(),
            });
            continue;
        }

        let phi = entry.phi.unwrap_or_else(|| {
            let signals = FieldSignals {
                relevance: if task != "general" && path.contains(&task) {
                    0.8
                } else {
                    0.3
                },
                ..Default::default()
            };
            field.compute_phi(&signals)
        });

        let view_costs = entry
            .view_costs
            .clone()
            .unwrap_or_else(|| ViewCosts::from_full_tokens(entry.original_tokens));

        let recommended_view = policies
            .recommended_view(path, seen_before, entry.original_tokens)
            .unwrap_or_else(|| {
                if intent_mode != "auto" && intent_mode != "reference" {
                    ViewKind::parse(intent_mode)
                } else {
                    field.select_view(&view_costs, temperature)
                }
            });

        let estimated_tokens = view_costs.get(&recommended_view);
        total_estimated += estimated_tokens;

        plan_items.push(PlanItem {
            path: path.clone(),
            recommended_view: recommended_view.as_str().to_string(),
            estimated_tokens,
            phi,
            state: format!("{effective_state:?}"),
            reason: format!("profile:{profile}"),
        });
    }

    let loaded_paths: Vec<String> = ledger.entries.iter().map(|e| e.path.clone()).collect();
    let (target_files, keywords) = crate::core::task_relevance::parse_task_hints(&task);
    let classification = crate::core::intent_engine::classify(&task);
    let structured = crate::core::intent_engine::StructuredIntent {
        task_type: classification.task_type,
        confidence: classification.confidence,
        targets: target_files,
        keywords,
        scope: crate::core::intent_engine::IntentScope::MultiFile,
        language_hint: None,
        urgency: 0.5,
        action_verb: None,
    };
    let deficit = crate::core::context_deficit::detect_deficit(ledger, &structured, &loaded_paths);

    for suggestion in &deficit.suggested_files {
        if !plan_items.iter().any(|p| p.path == suggestion.path) {
            plan_items.push(PlanItem {
                path: suggestion.path.clone(),
                recommended_view: suggestion.recommended_mode.clone(),
                estimated_tokens: suggestion.estimated_tokens,
                phi: 0.5,
                state: "suggested".to_string(),
                reason: format!("{:?}", suggestion.reason),
            });
            total_estimated += suggestion.estimated_tokens;
        }
    }

    plan_items.sort_by(|a, b| {
        b.phi
            .partial_cmp(&a.phi)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if total_estimated > budget_tokens {
        degrade_views(&mut plan_items, budget_tokens, &mut total_estimated);
    }

    format_plan(&task, budget_tokens, total_estimated, &plan_items, &profile)
}

/// Convert the plan into compile candidates for use with the compiler.
#[must_use]
pub fn plan_to_candidates(ledger: &ContextLedger, policies: &PolicySet) -> Vec<CompileCandidate> {
    let field = ContextField::new();
    let mut candidates = Vec::new();

    for entry in &ledger.entries {
        let path = &entry.path;
        let seen_before = true;
        let effective_state = policies.effective_state(
            path,
            entry.state.unwrap_or(ContextState::Included),
            seen_before,
            entry.original_tokens,
        );

        let phi = entry.phi.unwrap_or_else(|| {
            let signals = FieldSignals {
                relevance: 0.3,
                ..Default::default()
            };
            field.compute_phi(&signals)
        });

        let view_costs = entry
            .view_costs
            .clone()
            .unwrap_or_else(|| ViewCosts::from_full_tokens(entry.original_tokens));

        let item_id = entry
            .id
            .clone()
            .unwrap_or_else(|| ContextItemId::from_file(path));

        candidates.push(CompileCandidate {
            id: item_id,
            kind: entry.kind.unwrap_or(ContextKind::File),
            path: path.clone(),
            state: effective_state,
            phi,
            view_costs: view_costs.clone(),
            selected_view: entry.active_view.unwrap_or(ViewKind::Full),
            selected_tokens: entry.sent_tokens,
            pinned: effective_state == ContextState::Pinned,
            // Content fingerprint for redundancy/MMR (#5): the ledger's content
            // hash identifies byte-identical items so duplicates collapse to one;
            // distinct content yields no false overlap (word-Jaccard over a single
            // hash token). Falls back to the path when no hash is recorded.
            content_sketch: entry.source_hash.clone(),
        });
    }

    candidates
}

#[derive(Debug)]
struct PlanItem {
    path: String,
    recommended_view: String,
    estimated_tokens: usize,
    phi: f64,
    state: String,
    reason: String,
}

fn format_plan(
    task: &str,
    budget: usize,
    estimated: usize,
    items: &[PlanItem],
    profile: &str,
) -> String {
    let utilization = if budget > 0 {
        estimated as f64 / budget as f64 * 100.0
    } else {
        0.0
    };

    let mut out = String::new();
    out.push_str(&format!("[ctx_plan] task=\"{task}\" profile={profile}\n"));
    out.push_str(&format!(
        "Budget: {estimated}/{budget} tokens ({utilization:.1}% estimated)\n\n"
    ));

    let included: Vec<_> = items.iter().filter(|i| i.state != "excluded").collect();
    let excluded: Vec<_> = items.iter().filter(|i| i.state == "excluded").collect();

    if !included.is_empty() {
        out.push_str("Planned items:\n");
        for item in &included {
            let default_reason = format!("profile:{profile}");
            let extra = if item.reason.is_empty() || item.reason == default_reason {
                String::new()
            } else {
                format!(" {}", item.reason)
            };
            out.push_str(&format!(
                "  {} {} {}t phi={:.2} [{}]{}\n",
                item.path,
                item.recommended_view,
                item.estimated_tokens,
                item.phi,
                item.state,
                extra
            ));
        }
    }

    if !excluded.is_empty() {
        out.push_str(&format!("\nExcluded ({}):\n", excluded.len()));
        for item in &excluded {
            out.push_str(&format!("  {} — {}\n", item.path, item.reason));
        }
    }

    if utilization > 90.0 {
        out.push_str(
            "\nWARNING: Estimated tokens exceed 90% of budget. Consider stricter views.\n",
        );
    }

    out
}

fn degrade_views(items: &mut [PlanItem], budget: usize, total: &mut usize) {
    let degrade_order: &[(&str, &str, f64)] = &[
        ("full", "map", 0.3),
        ("map", "signatures", 0.5),
        ("signatures", "signatures", 0.7),
    ];

    for &(from, to, ratio) in degrade_order {
        if *total <= budget {
            break;
        }
        let mut candidates: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.recommended_view == from && it.state != "excluded" && it.state != "Pinned"
            })
            .map(|(i, _)| i)
            .collect();
        candidates.sort_by(|&a, &b| {
            items[a]
                .phi
                .partial_cmp(&items[b].phi)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for idx in candidates {
            if *total <= budget {
                break;
            }
            let old_tokens = items[idx].estimated_tokens;
            let new_tokens = (old_tokens as f64 * ratio) as usize;
            *total = total.saturating_sub(old_tokens) + new_tokens;
            items[idx].estimated_tokens = new_tokens;
            items[idx].recommended_view = to.to_string();
            items[idx].reason = format!("degraded:{from}->{to}");
        }
    }
}

fn get_str(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    args?
        .get(key)?
        .as_str()
        .map(std::string::ToString::to_string)
}
