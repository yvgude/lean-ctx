// SPDX-License-Identifier: Apache-2.0

//! Runtime integration helpers for the Context Control Kernel.

pub(crate) mod runtime;

use super::autopilot::{
    AdaptiveLearningState, AutopilotController, AutopilotDecision, AutopilotEconomics,
    AutopilotInput, PreloadBudget, UserOverrides,
};
use super::enforce::{KernelMode, resolve_mode};
use super::policy::ContextPolicy;
use super::types::{ContextPlanV1, ContextReceiptV1, PlanEntry, ReceiptOutcome, RetrievalContext};

/// Result of kernel gating: what to add and what to suppress.
#[derive(Debug, Clone)]
pub struct KernelVerdict {
    /// Cross-store context to supplement (hard-capped, never exceeds budget).
    pub supplement: Option<String>,
    /// Content identifiers that are already in context and should not be resent.
    pub suppress: Vec<String>,
    /// Tokens consumed by the kernel supplement.
    pub budget_used: usize,
}
/// Result of kernel enrichment for compose integration.
#[derive(Debug, Clone)]
pub struct KernelEnrichment {
    /// Legacy selection-plan view; not a task-bound execution or learning proof.
    pub plan: ContextPlanV1,
    autopilot_decision: Option<AutopilotDecision>,
    /// Human-readable blocks suitable for compose output injection.
    pub blocks: String,
    /// Gate decision accompanying the backward-compatible blocks.
    pub verdict: KernelVerdict,
    /// Kernel policy mode used while producing this enrichment.
    pub enforced_mode: KernelMode,
}

impl KernelEnrichment {
    /// Inspect the canonical planning decision behind this context supplement.
    /// Other proposed policies are not evidence that a surface executed them.
    pub fn autopilot_decision(&self) -> Option<&AutopilotDecision> {
        self.autopilot_decision.as_ref()
    }
}
/// Check whether `path` should be suppressed as already delivered.
/// Returns `false` until the orchestrator exposes recent-delivery state.
pub fn kernel_gate(_path: &str, _project_root: &str) -> bool {
    false
}
/// Enrich a compose response with kernel-selected context.
///
/// Returns cross-store context that the compose pipeline misses, or `None`.
pub fn kernel_enrich(
    task: &str,
    project_root: &str,
    budget_tokens: usize,
) -> Option<KernelEnrichment> {
    match runtime::current_handoff() {
        runtime::KernelPlanningHandoff::Legacy => {}
        runtime::KernelPlanningHandoff::Suppressed => return None,
        runtime::KernelPlanningHandoff::Prepared(prepared) => {
            return prepared.enrich(task, project_root, budget_tokens);
        }
    }
    let capped_budget = budget_tokens.min(150);
    if capped_budget == 0 {
        return None;
    }
    let ctx = RetrievalContext {
        query: task.to_owned(),
        task: Some(task.to_owned()),
        project_root: project_root.to_owned(),
        budget: crate::core::context_field::TokenBudget {
            total: capped_budget,
            used: 0,
        },
        max_candidates: 20,
    };
    let mode = resolve_mode(project_root);
    let input = reference_input(ctx, mode).ok()?;
    let decision = AutopilotController::for_project(project_root)
        .plan(&input, None)
        .ok()?;
    enrichment_from_decision(decision, capped_budget, mode)
}

fn enrichment_from_decision(
    decision: AutopilotDecision,
    capped_budget: usize,
    mode: KernelMode,
) -> Option<KernelEnrichment> {
    let plan = decision.context_plan.clone();
    let enrichments: Vec<&PlanEntry> = plan
        .selected
        .iter()
        .filter(|entry| entry.provider != "context.ledger")
        .collect();

    let blocks = format_enrichment_blocks(&enrichments);
    enrichment_from_plan(plan, blocks, capped_budget, mode).map(|mut enrichment| {
        enrichment.autopilot_decision = Some(decision);
        enrichment
    })
}

/// This legacy surface has no paid entitlement, outcome history, or cost evidence.
/// Use the canonical reference planner without inventing those inputs or a TaskId.
pub(crate) fn reference_input(
    retrieval: RetrievalContext,
    kernel_mode: KernelMode,
) -> Result<AutopilotInput, super::policy::PolicyLoadError> {
    let policy = ContextPolicy::from_config(&retrieval.project_root)?;
    Ok(AutopilotInput {
        retrieval,
        evaluation_time: None,
        task_class: crate::core::outcome::contracts::TaskClass::Investigation,
        entitled_to_adaptive: false,
        confidence_milli: 0,
        configured_mode: None,
        default_mode: "full".to_owned(),
        security_forced_mode: None,
        overrides: UserOverrides {
            no_routing: true,
            no_telemetry: true,
            ..UserOverrides::default()
        },
        policy,
        kernel_mode,
        economics: AutopilotEconomics::default(),
        learning: AdaptiveLearningState::default(),
        available_providers: Vec::new(),
        local_providers: Default::default(),
        cached_preloads: Default::default(),
        preload_budget: PreloadBudget {
            max_items: 0,
            ..PreloadBudget::default()
        },
        // A promoted policy belongs to one tenant/project scope; only callers
        // that know the task's scope (its envelope) may attach it.
        context_policy: None,
    })
}

pub(super) fn enrichment_from_plan(
    plan: ContextPlanV1,
    blocks: String,
    budget: usize,
    enforced_mode: KernelMode,
) -> Option<KernelEnrichment> {
    let verdict = verdict_from_blocks(blocks, budget);
    let blocks = verdict.supplement.clone()?;
    Some(KernelEnrichment {
        plan,
        autopilot_decision: None,
        blocks,
        verdict,
        enforced_mode,
    })
}

fn verdict_from_blocks(blocks: String, budget: usize) -> KernelVerdict {
    let blocks = truncate_to_token_budget(blocks, budget);
    let supplement = (!blocks.is_empty()).then(|| blocks.clone());
    let budget_used = supplement
        .as_deref()
        .map_or(0, crate::core::tokens::count_tokens);
    KernelVerdict {
        supplement,
        suppress: Vec::new(),
        budget_used,
    }
}

fn truncate_to_token_budget(mut text: String, budget: usize) -> String {
    if crate::core::tokens::count_tokens(&text) <= budget {
        return text;
    }
    let mut low = 0;
    let mut high = text.len();
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        let boundary = text.floor_char_boundary(middle);
        if crate::core::tokens::count_tokens(&text[..boundary]) <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    text.truncate(text.floor_char_boundary(low));
    // #1993: whole entries only. A byte cut left a bullet ending mid-path
    // (`files:[/Users/<me>`), a record that no longer described itself.
    text.truncate(text.rfind('\n').map_or(0, |end| end + 1));
    text
}

fn format_enrichment_blocks(entries: &[&PlanEntry]) -> String {
    let mut out = String::new();
    append_provider_block(&mut out, entries, "knowledge.facts", "Relevant Knowledge");
    append_provider_block(&mut out, entries, "memory.episodic", "Relevant Episodes");
    append_provider_block(
        &mut out,
        entries,
        "memory.procedural",
        "Relevant Procedures",
    );
    append_provider_block(&mut out, entries, "session.state", "Relevant Session State");
    out
}

fn append_provider_block(
    output: &mut String,
    entries: &[&PlanEntry],
    provider: &str,
    heading: &str,
) {
    let mut found = false;
    for entry in entries
        .iter()
        .copied()
        .filter(|entry| entry.provider == provider)
        .filter(|entry| !entry.reason.trim().is_empty())
        .filter(|entry| entry.reason != "selected by compiler")
    {
        if !found {
            output.push_str("\n## ");
            output.push_str(heading);
            output.push('\n');
            found = true;
        }
        output.push_str("- ");
        output.push_str(&entry.reason);
        output.push_str(" (phi=");
        output.push_str(&format!("{:.2}", entry.phi));
        output.push_str(")\n");
    }
}

/// Update the bandit-learned FieldWeights based on a receipt outcome.
///
/// Accepted outcomes reinforce the balanced arm, rejected outcomes penalize
/// the aggressive arm, and partial outcomes inform the conservative arm.
pub fn apply_feedback(receipt: &ContextReceiptV1) {
    use crate::core::context_field::{FieldWeights, set_active_weights};

    let arm_name = match receipt.outcome {
        ReceiptOutcome::Accepted => "balanced",
        ReceiptOutcome::Rejected => "aggressive",
        ReceiptOutcome::Partial => "conservative",
        ReceiptOutcome::Unknown => return,
    };
    let mut bandit = crate::core::bandit::ThresholdBandit::default();
    bandit.update(arm_name, receipt.outcome == ReceiptOutcome::Accepted);

    let best_idx = bandit.best_arm_idx_by_mean();
    if let Some(best_arm) = bandit.arms.get(best_idx) {
        set_active_weights(FieldWeights::from_arm(best_arm));
    }
}

/// Format a plan as a compact human-readable summary.
pub fn format_plan_summary(plan: &ContextPlanV1) -> String {
    let mut out = String::new();
    let plan_prefix = &plan.plan_id[..plan.plan_id.len().min(8)];
    out.push_str(&format!(
        "[kernel] plan={plan_prefix} intent=\"{}\" budget={}/{}\n",
        plan.intent, plan.budget.used_tokens, plan.budget.total_tokens,
    ));
    out.push_str(&format!(
        "  selected={} excluded={} deferred={}\n",
        plan.selected.len(),
        plan.excluded.len(),
        plan.deferred.len(),
    ));

    let mut providers: Vec<_> = plan.provider_stats.iter().collect();
    providers.sort_unstable_by_key(|(k, _)| *k);
    for (provider, stat) in providers {
        out.push_str(&format!(
            "  {provider}: {}/{} candidates, {} tokens\n",
            stat.candidates_selected, stat.candidates_offered, stat.tokens_used,
        ));
    }
    out
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;

    use crate::core::{
        context_kernel::autopilot::{AdaptiveLearningState, PlannerTier},
        data_dir::isolated_data_dir,
        knowledge::ProjectKnowledge,
        memory_policy::MemoryPolicy,
    };

    use super::super::enforce::KernelMode;
    use super::super::types::PlanBudget;
    use super::{
        ContextPlanV1, PlanEntry, enrichment_from_plan, format_enrichment_blocks, kernel_gate,
        truncate_to_token_budget, verdict_from_blocks,
    };

    fn plan(selected: Vec<PlanEntry>) -> ContextPlanV1 {
        ContextPlanV1 {
            plan_id: "plan".to_owned(),
            intent: "test".to_owned(),
            budget: PlanBudget {
                total_tokens: 150,
                used_tokens: 0,
                remaining_tokens: 150,
            },
            selected,
            excluded: Vec::new(),
            deferred: Vec::new(),
            provider_stats: HashMap::new(),
            origins: Default::default(),
        }
    }

    fn entry(reason: &str) -> PlanEntry {
        PlanEntry {
            object_id: "fact".to_owned(),
            provider: "knowledge.facts".to_owned(),
            view: "summary".to_owned(),
            tokens: 1,
            phi: 0.8,
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn budget_capped_at_150() {
        let item = entry(&"token ".repeat(1_000));
        let blocks = format_enrichment_blocks(&[&item]);
        let enrichment = enrichment_from_plan(plan(vec![item]), blocks, 150, KernelMode::Shadow)
            .expect("long enrichment should be truncated, not removed");
        assert!(enrichment.verdict.budget_used <= 150);
    }
    #[test]
    fn truncation_keeps_whole_entries_only() {
        let line = "- [success] files:[/Users/me/htdocs/evcc/docs/agents/a.md] (phi=0.25)\n";
        let text = format!("\n## Relevant Episodes\n{}", line.repeat(40));
        let cut = truncate_to_token_budget(text, 150);
        assert!(!cut.is_empty());
        assert!(cut.ends_with('\n'), "a partial entry survived: {cut:?}");
        assert!(cut.lines().skip(2).all(|l| l == line.trim_end()));
    }

    #[test]
    fn empty_supplement_when_no_candidates() {
        let verdict = verdict_from_blocks(String::new(), 150);
        assert!(verdict.supplement.is_none());
        assert_eq!(verdict.budget_used, 0);
    }

    #[test]
    fn compiler_selection_placeholders_are_omitted() {
        let placeholder = entry("selected by compiler");

        assert!(format_enrichment_blocks(&[&placeholder]).is_empty());
    }
    #[test]
    fn verdict_has_correct_budget_used() {
        let item = entry("Known constraint");
        let blocks = format_enrichment_blocks(&[&item]);
        let enrichment = enrichment_from_plan(plan(vec![item]), blocks, 150, KernelMode::Shadow)
            .expect("entry should produce enrichment");
        assert_eq!(
            enrichment.verdict.budget_used,
            crate::core::tokens::count_tokens(enrichment.verdict.supplement.as_deref().unwrap())
        );
    }
    #[test]
    fn kernel_gate_returns_false_by_default() {
        assert!(!kernel_gate("src/lib.rs", "/project"));
    }
    #[test]
    fn backward_compat_enrichment_still_works() {
        let item = entry("Known constraint");
        let blocks = format_enrichment_blocks(&[&item]);
        let enrichment = enrichment_from_plan(plan(vec![item]), blocks, 150, KernelMode::Shadow)
            .expect("entry should produce enrichment");
        assert_eq!(
            enrichment.blocks,
            enrichment
                .verdict
                .supplement
                .as_deref()
                .expect("legacy supplement")
        );
        assert_eq!(enrichment.enforced_mode, KernelMode::Shadow);
        assert!(enrichment.autopilot_decision().is_none());
    }

    #[test]
    fn production_enrichment_uses_canonical_community_planning() {
        let _data_dir = isolated_data_dir();
        let root = tempfile::tempdir().expect("project root");
        let project_root = root.path().to_str().expect("UTF-8 project root");
        let mut knowledge = ProjectKnowledge::new(project_root);
        knowledge.remember(
            "architecture",
            "renderer",
            "Renderer uses typed immutable context decisions.",
            "bridge-test",
            1.0,
            &MemoryPolicy::default(),
        );
        knowledge.save().expect("persist real project knowledge");

        let first = super::kernel_enrich("renderer context decisions", project_root, 500)
            .expect("persisted knowledge is selected and delivered");
        let second = super::kernel_enrich("renderer context decisions", project_root, 500)
            .expect("same project state is available");
        let decision = first
            .autopilot_decision()
            .expect("canonical decision retained");
        assert_eq!(decision.tier, PlannerTier::Community);
        assert!(decision.preloads.is_empty());
        assert!(
            decision
                .reasons
                .iter()
                .any(|reason| reason.code == "community_reference")
        );
        assert_eq!(first.plan.plan_id, decision.context_plan.plan_id);
        assert!(first.plan.budget.total_tokens <= 150);
        assert!(first.verdict.budget_used <= 150);
        assert!(first.blocks.contains("typed immutable context decisions"));
        assert_eq!(
            first.blocks,
            first.verdict.supplement.as_deref().expect("supplement")
        );
        assert_eq!(first.blocks, second.blocks);
        assert_eq!(
            decision
                .canonical_bytes()
                .expect("first canonical decision"),
            second
                .autopilot_decision()
                .expect("second decision")
                .canonical_bytes()
                .expect("second canonical decision")
        );
    }

    #[test]
    fn reference_adapter_cannot_enable_personalized_or_speculative_work() {
        let _data_dir = isolated_data_dir();
        let input = super::reference_input(
            super::RetrievalContext {
                query: "context lookup".to_owned(),
                task: None,
                project_root: "/not-used-by-this-test".to_owned(),
                budget: crate::core::context_field::TokenBudget {
                    total: 150,
                    used: 0,
                },
                max_candidates: 20,
            },
            KernelMode::Shadow,
        )
        .expect("isolated default policy");
        assert!(!input.entitled_to_adaptive);
        assert_eq!(input.learning, AdaptiveLearningState::default());
        assert!(input.available_providers.is_empty());
        assert_eq!(input.preload_budget.max_items, 0);
        assert!(input.overrides.no_routing);
        assert!(input.overrides.no_telemetry);
    }

    #[test]
    fn zero_budget_does_not_produce_context_or_a_decision() {
        assert!(super::kernel_enrich("unused", "/not-used-by-zero-budget", 0).is_none());
    }
}
