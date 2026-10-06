//! End-to-end wiring from task triage through execution-value assessment.

use lean_ctx_protocol::{
    AcceptanceState, RiskClass, TaskComplexity, TaskEnvelopeV1, TaskProfileV1, TaskScope,
};

use crate::core::{
    execution_lifecycle::{
        CompletionObservation, ExecutionLifecycle, LifecycleStage, ProductEntitlements,
        RuntimeContext, TaskContext, ToolRequest, ToolSurface,
    },
    triage::{
        TriageEngine,
        profile::{TaskProfileLocal, TaskScopeLocal},
    },
    value_gate::{self, ExecutionCost, OutcomeSignal, TaskOutcome, ValueAssessment, ValueGate},
};

#[derive(Debug, Clone)]
/// Orchestrates triage, spine, and value gate for a complete task evaluation.
pub struct DecisionLoop {
    lifecycle: ExecutionLifecycle,
}

#[derive(Debug, Clone)]
/// Holds the task state and value assessment from a decision-loop execution.
pub struct DecisionResult {
    pub task_id: String,
    /// The ingress envelope enriched by triage; its task id is the lineage root.
    pub envelope: TaskEnvelopeV1,
    pub profile: TaskProfileLocal,
    pub envelope_created: bool,
    pub cost: Option<ExecutionCost>,
    pub outcome: Option<TaskOutcome>,
    pub assessment: Option<ValueAssessment>,
    pub acceptance_state: AcceptanceState,
    lifecycle_context: TaskContext,
}

impl Default for DecisionLoop {
    fn default() -> Self {
        Self::new(TriageEngine::default(), ValueGate)
    }
}

impl DecisionLoop {
    pub fn new(triage: TriageEngine, value_gate: ValueGate) -> Self {
        Self {
            lifecycle: ExecutionLifecycle::with_components(triage, value_gate),
        }
    }

    pub fn execute_task(&self, query: &str, session_id: &str, agent_id: &str) -> DecisionResult {
        let lifecycle_context = self.lifecycle.begin(
            ToolRequest {
                tool_name: "decision_loop".to_owned(),
                query: Some(query.to_owned()),
                session_id: session_id.to_owned(),
                agent_id: agent_id.to_owned(),
                surface: ToolSurface::Cli,
                idempotency_key: None,
            },
            RuntimeContext::default(),
            ProductEntitlements::default(),
        );
        lifecycle_context.advance_through(LifecycleStage::GatherContextStrategy);
        let _ = lifecycle_context.skip(LifecycleStage::AskAutopilot, "not entitled");
        let profile = lifecycle_context.profile.clone();
        let envelope = lifecycle_context.envelope.clone();

        DecisionResult {
            task_id: envelope.task_id.as_str().to_owned(),
            envelope,
            profile,
            envelope_created: true,
            cost: None,
            outcome: None,
            assessment: None,
            acceptance_state: AcceptanceState::Unknown,
            lifecycle_context,
        }
    }

    pub fn complete_task(
        &self,
        result: &mut DecisionResult,
        cost: ExecutionCost,
        signals: Vec<OutcomeSignal>,
    ) {
        let success = !signals.iter().any(|signal| {
            matches!(
                signal,
                OutcomeSignal::CompileError | OutcomeSignal::TestFailed
            )
        });
        let outcome = TaskOutcome {
            task_id: result.task_id.clone(),
            completed: true,
            signals: signals.clone(),
        };
        let lifecycle_outcome = self.lifecycle.complete(
            &result.lifecycle_context,
            CompletionObservation {
                input_tokens: cost.input_tokens,
                output_tokens: cost.output_tokens,
                model: cost.model.clone(),
                provider: cost.provider.clone(),
                success,
                outcome_signals: signals,
                shadow_auto_record: false,
                shadow_tokens: None,
                proxy_economics: None,
                heatmap: None,
            },
        );
        result.cost = Some(cost);
        result.outcome = Some(outcome);
        result.acceptance_state = lifecycle_outcome.accepted_outcome.accepted;
        result.assessment = lifecycle_outcome.assessment;
    }

    /// Fixture-only completion: never finalizes the production lifecycle or
    /// records value/causal evidence. Its output must be labeled simulated.
    pub fn complete_simulated_task(
        &self,
        result: &mut DecisionResult,
        cost: ExecutionCost,
        signals: Vec<OutcomeSignal>,
    ) {
        let outcome = TaskOutcome {
            task_id: result.task_id.clone(),
            completed: true,
            signals,
        };
        result.acceptance_state = crate::core::execution_lifecycle::evaluate_outcome_signals(
            &result.task_id,
            &result.profile.intent,
            &outcome.signals,
        )
        .accepted;
        let assessment = (result.acceptance_state != AcceptanceState::Unknown
            && !outcome.signals.is_empty())
        .then(|| value_gate::assess_simulated_task(&result.task_id, &cost, &outcome));
        result.cost = Some(cost);
        result.outcome = Some(outcome);
        result.assessment = assessment;
    }

    pub fn aggregate_cpao(results: &[DecisionResult]) -> Option<u64> {
        let (costs, accepted): (Vec<_>, Vec<_>) = results
            .iter()
            .filter_map(|result| {
                Some((
                    result.cost.as_ref()?.estimated_cost_micros,
                    result.assessment.as_ref()?.outcome_accepted,
                ))
            })
            .unzip();
        value_gate::cpao::cost_per_accepted_outcome(&costs, &accepted)
    }
}

pub(crate) fn protocol_profile(profile: &TaskProfileLocal) -> TaskProfileV1 {
    TaskProfileV1 {
        primary_intent: profile.intent.clone(),
        task_class: profile.task_class.clone(),
        complexity: match profile.complexity.as_str() {
            "medium" => TaskComplexity::Medium,
            "high" => TaskComplexity::High,
            _ => TaskComplexity::Low,
        },
        scope: match profile.scope {
            TaskScopeLocal::SingleFile => TaskScope::SingleFile,
            TaskScopeLocal::MultiFile => TaskScope::MultiFile,
            TaskScopeLocal::CrossModule => TaskScope::CrossModule,
            TaskScopeLocal::CrossProject => TaskScope::CrossProject,
        },
        context_need_milli: profile.context_need_milli,
        reasoning_need_milli: profile.reasoning_need_milli,
        risk_signal: match profile.risk_signal_milli {
            750.. => RiskClass::High,
            400.. => RiskClass::Medium,
            _ => RiskClass::Low,
        },
        confidence_milli: profile.confidence_milli,
        capability_id: None,
        capability_version: None,
        keywords: Vec::new(),
        language_hints: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{task_spine::TaskSpine, value_gate::cost_tracker::calculate_cost};

    fn cost() -> ExecutionCost {
        ExecutionCost {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 0,
            model: "gpt-4o".into(),
            provider: "openai".into(),
            estimated_cost_micros: calculate_cost(1_000, 500, 0, "gpt-4o"),
        }
    }

    #[test]
    fn legacy_decision_loop_does_not_invent_an_autopilot_decision() {
        use crate::core::execution_lifecycle::{LIFECYCLE_STAGE_ORDER, StageDisposition};

        let loop_ = DecisionLoop::default();
        let mut result = loop_.execute_task("inspect lifecycle", "session", "agent");
        assert_eq!(
            result.lifecycle_context.completed_stages(),
            LIFECYCLE_STAGE_ORDER[..6]
        );
        assert_eq!(
            result.lifecycle_context.stage_executions()[5].disposition,
            StageDisposition::Skipped("not entitled")
        );
        loop_.complete_task(&mut result, cost(), Vec::new());
        assert_eq!(
            result.lifecycle_context.completed_stages(),
            LIFECYCLE_STAGE_ORDER
        );
        assert_eq!(
            result.lifecycle_context.stage_executions()[5].disposition,
            StageDisposition::Skipped("not entitled")
        );
        assert_eq!(result.acceptance_state, AcceptanceState::Unknown);
    }

    #[test]
    fn test_full_decision_loop() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let loop_ = DecisionLoop::default();
        let mut result = loop_.execute_task("fix bug in auth.rs", "session", "agent");
        loop_.complete_task(
            &mut result,
            cost(),
            vec![OutcomeSignal::BuildSucceeded, OutcomeSignal::TestsPassed],
        );

        assert_eq!(result.profile.intent, "coding_fix");
        assert!(result.envelope_created);
        assert_eq!(result.envelope.task_id.as_str(), result.task_id);
        assert_eq!(
            TaskSpine::task_id().as_deref(),
            Some(result.task_id.as_str())
        );
        assert_eq!(result.cost.as_ref().unwrap().input_tokens, 1_000);
        assert_eq!(result.cost.as_ref().unwrap().output_tokens, 500);
        assert_eq!(result.cost.as_ref().unwrap().model, "gpt-4o");
        assert!(result.assessment.unwrap().cpao_micros.is_some());
    }

    #[test]
    fn test_rejected_outcome() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let loop_ = DecisionLoop::default();
        let mut result = loop_.execute_task("fix bug in auth.rs", "session", "agent");
        loop_.complete_task(&mut result, cost(), vec![OutcomeSignal::TestFailed]);

        assert!(!result.assessment.as_ref().unwrap().outcome_accepted);
        assert_eq!(result.assessment.unwrap().cpao_micros, None);
    }

    #[test]
    fn test_unknown_query() {
        let result = DecisionLoop::default().execute_task("", "session", "agent");

        assert_eq!(
            result.profile.confidence_milli,
            crate::core::triage::confidence::RULES_FALLBACK_MILLI
        );
        assert!(result.envelope_created);
    }

    #[test]
    fn test_multiple_tasks_cpao() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let loop_ = DecisionLoop::default();
        let mut results: Vec<_> = (0..3)
            .map(|index| {
                loop_.execute_task("fix bug in auth.rs", "session", &format!("agent-{index}"))
            })
            .collect();
        for result in &mut results {
            loop_.complete_task(
                result,
                cost(),
                vec![OutcomeSignal::BuildSucceeded, OutcomeSignal::TestsPassed],
            );
        }

        assert_eq!(
            DecisionLoop::aggregate_cpao(&results),
            Some(cost().estimated_cost_micros)
        );
    }
}
