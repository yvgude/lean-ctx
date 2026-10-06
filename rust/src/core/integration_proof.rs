//! Executable end-to-end proof for the task decision pipeline.

use std::sync::Arc;

use lean_ctx_protocol::AcceptanceState;

use super::{
    decision_loop::{DecisionLoop, DecisionResult, protocol_profile},
    evidence_ledger::EvidenceLedgerV1,
    knowledge_router::{
        ContextCandidate, KnowledgeRouter, PatternReferenceResolver,
        source_manifest::builtin_manifests,
    },
    value_gate::{ExecutionCost, OutcomeSignal, cost_tracker::calculate_cost},
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProofResult {
    pub tasks: Vec<TaskProof>,
    pub aggregate_cpao_micros: Option<u64>,
    pub total_cost_micros: u64,
    pub accepted_rate: f64,
    pub evidence_chain_complete: bool,
    /// In-memory ledger proving each stage for every task in this run.
    pub evidence_ledger: EvidenceLedgerV1,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskProof {
    pub task_id: String,
    pub query: String,
    pub profile_intent: String,
    pub profile_complexity: String,
    pub envelope_created: bool,
    pub references_found: Vec<String>,
    pub bundle_candidates: usize,
    pub receipt_sources: Vec<String>,
    pub cost_micros: u64,
    pub acceptance_state: AcceptanceState,
    pub outcome_accepted: bool,
    pub cpao_micros: Option<u64>,
    pub evidence_stages: Vec<String>,
}

pub fn prove_decision_loop() -> ProofResult {
    let loop_ = DecisionLoop::default();
    let router = KnowledgeRouter {
        manifests: builtin_manifests(),
        resolvers: vec![Arc::new(PatternReferenceResolver)],
    };
    let queries = [
        "Fix the authentication bug in src/auth/login.rs, users report LEAN-42",
        "Refactor the payment module for better error handling",
        "Add unit tests for the new triage engine",
        "Review PR #789 for security issues",
        "Deploy the staging environment with new config",
    ];
    let mut results = Vec::with_capacity(queries.len());
    let mut tasks = Vec::with_capacity(queries.len());
    let mut evidence_ledger = EvidenceLedgerV1::default();
    for (index, query) in queries.into_iter().enumerate() {
        let mut result = loop_.execute_task(query, "integration-proof", &format!("proof-{index}"));
        let routing = router.route(
            &result.task_id,
            query,
            &protocol_profile(&result.profile),
            &[],
            None,
        );
        let (cost, signals) = simulated_execution(index);
        loop_.complete_simulated_task(&mut result, cost, signals);
        let evidence_stages = record_evidence_chain(&mut evidence_ledger, &result, &routing);
        tasks.push(task_proof(
            query,
            &result,
            &routing.candidates,
            routing.receipt.sources_used,
            evidence_stages,
        ));
        results.push(result);
    }
    let total_cost_micros = tasks.iter().map(|task| task.cost_micros).sum();
    let accepted = tasks.iter().filter(|task| task.outcome_accepted).count();
    let evidence_chain_complete = tasks.iter().all(|task| {
        task.envelope_created
            && has_complete_stage_chain(&task.evidence_stages)
            && task.cost_micros > 0
            && task.cpao_micros.is_some() == task.outcome_accepted
    });
    ProofResult {
        aggregate_cpao_micros: DecisionLoop::aggregate_cpao(&results),
        total_cost_micros,
        accepted_rate: accepted as f64 / tasks.len() as f64,
        evidence_chain_complete,
        evidence_ledger,
        tasks,
    }
}

const EVIDENCE_STAGES: [&str; 4] = ["ingress", "triage", "router", "value_gate"];

fn has_complete_stage_chain(stages: &[String]) -> bool {
    stages.iter().map(String::as_str).eq(EVIDENCE_STAGES)
}

fn record_evidence_chain(
    ledger: &mut EvidenceLedgerV1,
    result: &DecisionResult,
    routing: &super::knowledge_router::RoutingResult,
) -> Vec<String> {
    let details = [
        result.envelope.intent.as_deref().unwrap_or_default(),
        &result.profile.intent,
        &routing.receipt.receipt_id,
        acceptance_label(result.acceptance_state),
    ];
    for (stage, detail) in EVIDENCE_STAGES.iter().zip(details) {
        ledger.record_manual_with_task(
            &format!("decision_loop.{stage}"),
            Some(detail),
            Some(&result.task_id),
            chrono::Utc::now(),
        );
    }
    ledger
        .items
        .iter()
        .filter(|item| item.task_id.as_deref() == Some(result.task_id.as_str()))
        .filter_map(|item| item.key.strip_prefix("decision_loop."))
        .map(str::to_owned)
        .collect()
}

fn simulated_execution(index: usize) -> (ExecutionCost, Vec<OutcomeSignal>) {
    let (input, output, model, signals) = match index {
        0 => (
            800,
            400,
            "gpt-4o",
            vec![OutcomeSignal::BuildSucceeded, OutcomeSignal::TestsPassed],
        ),
        1 => (
            2_000,
            1_200,
            "claude-sonnet",
            vec![
                OutcomeSignal::BuildSucceeded,
                OutcomeSignal::TestsPassed,
                OutcomeSignal::LintClean,
            ],
        ),
        2 => (600, 800, "gpt-4o", vec![OutcomeSignal::TestsPassed]),
        3 => (
            1_500,
            600,
            "claude-sonnet",
            vec![OutcomeSignal::UserAccepted],
        ),
        _ => (400, 200, "gpt-4o", vec![OutcomeSignal::CompileError]),
    };
    (
        ExecutionCost {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: 0,
            model: model.into(),
            provider: if model == "gpt-4o" {
                "openai"
            } else {
                "anthropic"
            }
            .into(),
            estimated_cost_micros: calculate_cost(input, output, 0, model),
        },
        signals,
    )
}

fn task_proof(
    query: &str,
    result: &DecisionResult,
    candidates: &[ContextCandidate],
    receipt_sources: Vec<String>,
    evidence_stages: Vec<String>,
) -> TaskProof {
    let cost = result.cost.as_ref().expect("completed decision has cost");
    TaskProof {
        task_id: result.task_id.clone(),
        query: query.into(),
        profile_intent: result.profile.intent.clone(),
        profile_complexity: result.profile.complexity.clone(),
        envelope_created: result.envelope_created,
        references_found: candidates
            .iter()
            .filter_map(|candidate| candidate.reference.clone())
            .collect(),
        bundle_candidates: candidates.len(),
        receipt_sources,
        cost_micros: cost.estimated_cost_micros,
        acceptance_state: result.acceptance_state,
        outcome_accepted: result.acceptance_state == AcceptanceState::Accepted,
        cpao_micros: result
            .assessment
            .as_ref()
            .and_then(|assessment| assessment.cpao_micros),
        evidence_stages,
    }
}

const fn acceptance_label(state: AcceptanceState) -> &'static str {
    match state {
        AcceptanceState::Accepted => "accepted",
        AcceptanceState::Rejected => "rejected",
        AcceptanceState::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keep fixture state isolated even though proof value assessment is
    // in-memory; other proof stages may still write process-local state.
    fn prove_decision_loop() -> ProofResult {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        super::prove_decision_loop()
    }

    #[test]
    fn test_proof_all_tasks_complete() {
        assert!(
            prove_decision_loop()
                .tasks
                .iter()
                .all(|task| task.envelope_created)
        );
    }

    #[test]
    fn test_proof_references_detected() {
        let proof = prove_decision_loop();
        assert!(proof.tasks[0].references_found.contains(&"LEAN-42".into()));
        assert!(proof.tasks[3].references_found.contains(&"#789".into()));
    }

    #[test]
    fn test_proof_profiles_correct() {
        let proof = prove_decision_loop();
        let intents: Vec<_> = proof
            .tasks
            .iter()
            .map(|task| task.profile_intent.as_str())
            .collect();
        assert_eq!(intents[0], "coding_fix");
        assert_eq!(intents[1], "refactor");
        assert!(
            ["test", "generate"].contains(&intents[2]),
            "task 3 should be test or generate, got: {}",
            intents[2]
        );
        assert_eq!(intents[3], "review");
        assert!(
            ["deploy", "config", "generate"].contains(&intents[4]),
            "task 5 should be deploy/config/generate, got: {}",
            intents[4]
        );
    }

    #[test]
    fn test_proof_cpao_calculated() {
        assert!(
            prove_decision_loop()
                .aggregate_cpao_micros
                .is_some_and(|cpao| cpao > 0)
        );
    }

    #[test]
    fn test_proof_evidence_chain() {
        let proof = prove_decision_loop();
        assert!(proof.evidence_chain_complete);
        assert!(
            proof
                .tasks
                .iter()
                .all(|task| { has_complete_stage_chain(&task.evidence_stages) })
        );
    }

    #[test]
    fn test_proof_accepted_rate() {
        assert!(prove_decision_loop().accepted_rate >= 0.6);
    }

    #[test]
    fn incomplete_test_addition_evidence_remains_unknown() {
        let proof = prove_decision_loop();
        assert_eq!(proof.tasks[2].acceptance_state, AcceptanceState::Unknown);
        assert!(proof.tasks[2].cpao_micros.is_none());
    }

    #[test]
    fn test_proof_bundles_non_empty() {
        let proof = prove_decision_loop();
        assert!(proof.tasks[0].bundle_candidates > 0 && proof.tasks[3].bundle_candidates > 0);
    }

    #[test]
    fn test_prove_output_contains_tasks() {
        let proof = prove_decision_loop();
        let output = crate::cli::prove::render(&proof, "table").unwrap();
        assert!(
            proof.tasks.iter().all(|task| {
                output.contains(&task.task_id.chars().take(11).collect::<String>())
            })
        );
    }

    #[test]
    fn test_prove_json_valid() {
        let output = crate::cli::prove::render(&prove_decision_loop(), "json").unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&output).is_ok());
    }

    #[test]
    fn test_proof_simulation_does_not_persist_fake_evidence() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let proof = super::prove_decision_loop();

        assert!(proof.evidence_chain_complete);
        assert!(!crate::core::value_gate::ValueGateStore::persist_path().exists());
        assert!(
            !crate::core::paths::data_dir()
                .unwrap()
                .join("policy_outcomes.jsonl")
                .exists()
        );
    }

    #[test]
    fn test_proof_preserves_existing_recorded_value_evidence() {
        use crate::core::value_gate::{TaskOutcome, ValueGate, ValueGateStore};

        let _isolation = crate::core::data_dir::isolated_data_dir();
        let (cost, signals) = simulated_execution(0);
        let task_id = "recorded-before-self-test";
        let assessment = ValueGate::evaluate_task(
            task_id,
            &cost,
            &TaskOutcome {
                task_id: task_id.into(),
                completed: true,
                signals,
            },
        );
        let ledger = ValueGateStore::persist_path();
        let before = std::fs::read(&ledger).unwrap();
        assert_eq!(ValueGateStore::load_from_disk(), vec![assessment.clone()]);

        let proof = super::prove_decision_loop();

        assert!(proof.evidence_chain_complete);
        assert_eq!(std::fs::read(&ledger).unwrap(), before);
        assert_eq!(ValueGateStore::load_from_disk(), vec![assessment]);
    }

    #[test]
    fn test_prove_outputs_are_labeled_simulated_self_test() {
        let proof = prove_decision_loop();
        for format in ["json", "table", "markdown"] {
            let output = crate::cli::prove::render(&proof, format).unwrap();
            assert!(output.contains("SIMULATED SELF-TEST ONLY"));
            assert!(output.contains("NOT ACTUAL PAID EXECUTION OR PRODUCTION ACCEPTANCE"));
        }

        let json: serde_json::Value =
            serde_json::from_str(&crate::cli::prove::render(&proof, "json").unwrap()).unwrap();
        assert_eq!(json["execution_mode"], "simulated_self_test");
        assert_eq!(
            json["disclaimer"],
            "NOT ACTUAL PAID EXECUTION OR PRODUCTION ACCEPTANCE"
        );
    }

    #[test]
    fn test_prove_evidence_complete() {
        assert!(prove_decision_loop().evidence_chain_complete);
    }

    #[test]
    fn e2e_propagates_task_id_through_every_stage() {
        let _isolation = crate::core::data_dir::isolated_data_dir();
        let loop_ = DecisionLoop::default();
        let mut result = loop_.execute_task("fix LEAN-42 in src/main.rs", "e2e", "agent");
        let router = KnowledgeRouter {
            manifests: builtin_manifests(),
            resolvers: vec![Arc::new(PatternReferenceResolver)],
        };
        let routing = router.route(
            &result.task_id,
            "fix LEAN-42 in src/main.rs",
            &protocol_profile(&result.profile),
            &[],
            None,
        );
        loop_.complete_task(
            &mut result,
            simulated_execution(0).0,
            vec![OutcomeSignal::BuildSucceeded, OutcomeSignal::TestsPassed],
        );

        assert_eq!(result.envelope.task_id.as_str(), result.task_id);
        assert_eq!(routing.bundle.task_id, result.task_id);
        assert_eq!(routing.receipt.task_id, result.task_id);
        assert_eq!(result.outcome.as_ref().unwrap().task_id, result.task_id);
        assert_eq!(result.assessment.as_ref().unwrap().task_id, result.task_id);
        assert!(has_complete_stage_chain(&record_evidence_chain(
            &mut EvidenceLedgerV1::default(),
            &result,
            &routing,
        )));
    }

    #[test]
    fn triage_and_router_handle_boundary_inputs() {
        let loop_ = DecisionLoop::default();
        let router = KnowledgeRouter {
            manifests: builtin_manifests(),
            resolvers: vec![Arc::new(PatternReferenceResolver)],
        };
        for query in [
            "",
            &"fix authentication regression ".repeat(20_000),
            "認証の不具合を修正してください LEAN-42",
        ] {
            let result = loop_.execute_task(query, "edge-cases", "agent");
            assert!(!result.task_id.is_empty());
            assert!(!result.profile.task_class.is_empty());
            assert!(!result.profile.intent.is_empty());
            let routing = router.route(
                &result.task_id,
                query,
                &protocol_profile(&result.profile),
                &[],
                None,
            );
            assert_eq!(routing.bundle.task_id, result.task_id);
            assert_eq!(routing.receipt.task_id, result.task_id);
        }
    }

    #[test]
    fn malformed_references_do_not_create_candidates() {
        let router = KnowledgeRouter {
            manifests: builtin_manifests(),
            resolvers: vec![Arc::new(PatternReferenceResolver)],
        };
        let profile = protocol_profile(
            &DecisionLoop::default()
                .execute_task("", "malformed", "agent")
                .profile,
        );
        let routing = router.route("task-malformed", "LEAN- # src/../.rs", &profile, &[], None);
        assert!(routing.candidates.is_empty());
        assert!(routing.receipt.sources_used.is_empty());
    }
}
