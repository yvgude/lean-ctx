//! `ctx_compile` -- Context compilation tool.
//!
//! Runs the context compiler to produce an optimal context package
//! under budget constraints. Uses the plan from `ctx_plan` or builds
//! one on-the-fly from the current ledger state.

use serde_json::Value;

use crate::core::context_compiler::{CompileMode, compile, format_compile_result};
use crate::core::context_field::TokenBudget;
use crate::core::context_handles::HandleRegistry;
use crate::core::context_ledger::ContextLedger;
use crate::core::context_policies::PolicySet;

const DEFAULT_BUDGET: usize = 12_000;

pub fn handle(
    args: Option<&serde_json::Map<String, Value>>,
    ledger: &ContextLedger,
    policies: &PolicySet,
) -> String {
    let mode_str = get_str(args, "mode").unwrap_or_else(|| "handles".to_string());
    let mode = CompileMode::parse(&mode_str);
    let budget_tokens: usize = args
        .and_then(|a| a.get("budget"))
        .and_then(serde_json::Value::as_u64)
        .map_or(DEFAULT_BUDGET, |b| b as usize);

    let budget = TokenBudget {
        total: budget_tokens,
        used: 0,
    };

    let candidates = crate::tools::ctx_plan::plan_to_candidates(ledger, policies);
    if candidates.is_empty() {
        return "[ctx_compile] no context items in ledger — nothing to compile".to_string();
    }

    let result = compile(&candidates, budget, mode);

    match mode {
        CompileMode::HandleManifest => {
            let mut registry = HandleRegistry::new();
            for item in &result.selected {
                let kind = candidates
                    .iter()
                    .find(|c| c.id.to_string() == item.id)
                    .map_or(crate::core::context_field::ContextKind::File, |c| c.kind);

                let view_costs = candidates
                    .iter()
                    .find(|c| c.id.to_string() == item.id)
                    .map(|c| &c.view_costs)
                    .cloned()
                    .unwrap_or_default();

                registry.register(
                    crate::core::context_field::ContextItemId(item.id.clone()),
                    kind,
                    &item.path,
                    &format!("{} {}", item.path, item.view),
                    &view_costs,
                    item.phi,
                    item.pinned,
                );
            }
            let mut out = registry.format_manifest(budget_tokens, result.budget_used);
            out.push_str(&format!(
                "\n\nRun ID: {} | Items: {} selected, {} excluded\n",
                result.run_id, result.items_selected, result.items_excluded
            ));
            out
        }
        CompileMode::Compressed | CompileMode::FullPrompt => format_compile_result(&result),
    }
}

fn get_str(args: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    args?
        .get(key)?
        .as_str()
        .map(std::string::ToString::to_string)
}
