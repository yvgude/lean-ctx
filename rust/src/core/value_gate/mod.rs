//! End-to-end task value assessment: execution cost → outcome → CPAO.

pub mod cost_tracker;
pub mod cpao;
pub mod outcome_evaluator;
pub mod report;
pub mod store;

pub use cost_tracker::ExecutionCost;
pub use outcome_evaluator::{OutcomeSignal, TaskOutcome};
pub use store::ValueGateStore;

/// Stateless orchestrator for the value-gate loop.
#[derive(Debug, Clone, Copy, Default)]
pub struct ValueGate;

pub fn store() -> &'static store::ValueGateStore {
    static STORE: std::sync::OnceLock<store::ValueGateStore> = std::sync::OnceLock::new();
    STORE.get_or_init(store::ValueGateStore::default)
}

/// Assessment produced after one execution and deterministic outcome check.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ValueAssessment {
    pub task_id: String,
    pub model: String,
    pub total_tokens: u64,
    pub cost_micros: u64,
    pub outcome_accepted: bool,
    pub cpao_micros: Option<u64>,
    pub evidence: Vec<String>,
    pub timestamp: String,
}

impl ValueGate {
    pub fn evaluate_task(
        task_id: &str,
        execution_cost: &ExecutionCost,
        outcome: &TaskOutcome,
    ) -> ValueAssessment {
        evaluate_task(task_id, execution_cost, outcome)
    }
}

/// Run the complete task → cost → outcome → CPAO path.
pub fn evaluate_task(
    task_id: &str,
    execution_cost: &ExecutionCost,
    outcome: &TaskOutcome,
) -> ValueAssessment {
    let task_id_matches = outcome.task_id == task_id;
    let outcome_accepted = task_id_matches && outcome_evaluator::evaluate(outcome);
    let cost_micros = execution_cost.estimated_cost_micros;
    let cpao_micros = cpao::cost_per_accepted_outcome(&[cost_micros], &[outcome_accepted]);
    let mut evidence = vec![
        format!("task_id_matches={task_id_matches}"),
        format!("execution_cost_micros={cost_micros}"),
        format!("outcome_completed={}", outcome.completed),
        format!("outcome_accepted={outcome_accepted}"),
    ];
    evidence.extend(
        outcome
            .signals
            .iter()
            .map(|signal| format!("signal={signal:?}")),
    );
    let causal_chunks = crate::core::causal_attribution::chunks_for_session(task_id);
    let causal_outcome = if outcome_accepted {
        crate::core::causal_attribution::Outcome::Success
    } else {
        crate::core::causal_attribution::Outcome::Failure
    };
    if let Err(error) = crate::core::causal_attribution::record_outcome(
        task_id,
        crate::core::causal_attribution::OutcomeSignal {
            session_id: task_id.to_owned(),
            outcome: causal_outcome,
            evidence: format!("value_gate outcome_accepted={outcome_accepted}"),
        },
    ) {
        tracing::debug!(%error, task_id, "causal attribution ValueGate recording failed");
    }
    evidence.push(format!("causal_chunks_present={}", causal_chunks.len()));
    evidence.extend(
        causal_chunks
            .iter()
            .map(|chunk| format!("causal_chunk id={} source={}", chunk.id, chunk.source)),
    );

    let assessment = ValueAssessment {
        task_id: task_id.to_owned(),
        model: execution_cost.model.clone(),
        total_tokens: execution_cost
            .input_tokens
            .saturating_add(execution_cost.output_tokens)
            .saturating_add(execution_cost.cache_read_tokens),
        cost_micros,
        outcome_accepted,
        cpao_micros,
        evidence,
        timestamp: chrono::Utc::now().to_rfc3339(),
    };
    store().record(&assessment);
    #[cfg(feature = "enterprise")]
    record_adaptive_policy_outcome(task_id, &assessment);
    assessment
}

/// Feeds every completed Value Gate assessment back into adaptive policy selection.
fn record_adaptive_policy_outcome(task_id: &str, assessment: &ValueAssessment) {
    let task_class = crate::core::task_spine::TaskSpine::current()
        .filter(|envelope| envelope.task_id.as_str() == task_id)
        .and_then(|envelope| envelope.task_class)
        .unwrap_or_else(|| "chat".to_owned());
    let policy = crate::proxy::adaptive_policy::best_policy_for(&task_class);

    // ExecutionCost holds post-compression token usage only. No baseline means
    // no defensible percentage, so preserve the feedback event with 0.0 savings.
    crate::proxy::adaptive_policy::record_value_gate_outcome(
        task_class,
        policy,
        assessment.outcome_accepted,
        0.0,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_gate_e2e() {
        let cost = ExecutionCost {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_tokens: 0,
            model: "gpt-4o".into(),
            provider: "openai".into(),
            estimated_cost_micros: cost_tracker::calculate_cost(1_000_000, 100_000, 0, "gpt-4o"),
        };
        let outcome = TaskOutcome {
            task_id: "task-e2e".into(),
            completed: true,
            signals: vec![OutcomeSignal::BuildSucceeded, OutcomeSignal::UserAccepted],
        };
        let assessment = evaluate_task("task-e2e", &cost, &outcome);
        assert_eq!(assessment.task_id, "task-e2e");
        assert!(assessment.outcome_accepted);
        assert_eq!(assessment.cpao_micros, Some(3_500_000));
        assert!(!assessment.timestamp.is_empty());
        assert!(!assessment.evidence.is_empty());
    }

    #[test]
    fn rejects_outcome_for_another_task() {
        let cost = ExecutionCost {
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            model: "gpt-4o".into(),
            provider: "openai".into(),
            estimated_cost_micros: 1,
        };
        let outcome = TaskOutcome {
            task_id: "other-task".into(),
            completed: true,
            signals: vec![OutcomeSignal::TestsPassed],
        };
        let assessment = evaluate_task("expected-task", &cost, &outcome);
        assert!(!assessment.outcome_accepted);
        assert_eq!(assessment.cpao_micros, None);
        assert!(
            assessment
                .evidence
                .contains(&"task_id_matches=false".into())
        );
    }

    #[test]
    fn value_gate_attributes_chunks_present_for_accepted_task() {
        let envelope = crate::core::task_spine::TaskSpine::create_envelope(
            "attribute context",
            "value-gate-causal",
            "test-agent",
        );
        let task_id = envelope.task_id.as_str().to_owned();
        crate::core::causal_attribution::record_chunk(
            &task_id,
            crate::core::causal_attribution::ContextChunkRecord::new(
                "useful tool result",
                "ctx_read src/lib.rs",
                4,
                1,
            ),
        )
        .unwrap();
        let cost = ExecutionCost {
            input_tokens: 10,
            output_tokens: 1,
            cache_read_tokens: 0,
            model: "test".into(),
            provider: "test".into(),
            estimated_cost_micros: 1,
        };
        let outcome = TaskOutcome {
            task_id: task_id.clone(),
            completed: true,
            signals: vec![OutcomeSignal::BuildSucceeded],
        };

        let assessment = evaluate_task(&task_id, &cost, &outcome);
        assert!(assessment.outcome_accepted);
        assert!(
            assessment
                .evidence
                .iter()
                .any(|item| item == "causal_chunks_present=1")
        );
        assert!(
            assessment
                .evidence
                .iter()
                .any(|item| item.contains("causal_chunk") && item.contains("ctx_read src/lib.rs"))
        );
    }

    #[test]
    #[cfg(feature = "enterprise")]
    fn value_gate_records_adaptive_policy_feedback() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut envelope = crate::core::task_spine::TaskSpine::create_envelope(
            "debug a failing build",
            "value-gate-feedback",
            "test-agent",
        );
        envelope.task_class = Some("debugging".into());
        let task_id = envelope.task_id.as_str().to_owned();
        crate::core::task_spine::TaskSpine::set(envelope);

        let cost = ExecutionCost {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 0,
            model: "gpt-4o".into(),
            provider: "test".into(),
            estimated_cost_micros: 1,
        };
        let outcome = TaskOutcome {
            task_id: task_id.clone(),
            completed: true,
            signals: vec![OutcomeSignal::BuildSucceeded],
        };

        let assessment = evaluate_task(&task_id, &cost, &outcome);
        let path = crate::core::paths::data_dir()
            .unwrap()
            .join("policy_outcomes.jsonl");
        let recorded: crate::proxy::adaptive_policy::PolicyOutcome = serde_json::from_str(
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .last()
                .unwrap(),
        )
        .unwrap();

        assert_eq!(recorded.task_class, "debugging");
        assert_eq!(
            recorded.policy_used,
            crate::proxy::adaptive_policy::select_policy("debugging")
        );
        assert_eq!(recorded.session_success, Some(assessment.outcome_accepted));
        assert_eq!(recorded.savings_pct, 0.0);
    }
}
