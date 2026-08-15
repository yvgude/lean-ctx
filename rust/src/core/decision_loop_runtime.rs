//! Non-blocking bridge between MCP tool execution and the decision loop.

use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use crate::core::{
    decision_loop::protocol_profile,
    task_spine::TaskSpine,
    triage::{TaskAnalysisInput, TriageEngine},
    value_gate::{
        ExecutionCost, OutcomeSignal, TaskOutcome, ValueGate, ValueGateStore,
        cost_tracker::calculate_cost,
    },
};

#[derive(Debug)]
/// Bridges MCP tool lifecycle events into decision-loop accounting.
pub struct DecisionLoopRuntime {
    triage: TriageEngine,
    value_gate_store: Arc<Mutex<ValueGateStore>>,
    task_profiles: Mutex<HashMap<String, crate::core::triage::profile::TaskProfileLocal>>,
}

#[derive(Debug)]
/// Tracks a tool invocation while its decision-loop work is in progress.
pub struct TaskContext {
    pub task_id: String,
    pub session_id: String,
    pub triage_class: String,
    pub profile_intent: String,
    pub profile_complexity: String,
    pub filtered_lines: usize,
    pub start_time: Instant,
}

impl DecisionLoopRuntime {
    pub fn get_or_init() -> &'static Self {
        static RUNTIME: OnceLock<DecisionLoopRuntime> = OnceLock::new();
        RUNTIME.get_or_init(|| Self {
            triage: TriageEngine::default(),
            value_gate_store: Arc::new(Mutex::new(ValueGateStore::default())),
            task_profiles: Mutex::new(HashMap::new()),
        })
    }

    /// Returns the most recently triaged profile for a session.
    pub fn profile_for_session(
        &self,
        session_id: &str,
    ) -> Option<crate::core::triage::profile::TaskProfileLocal> {
        self.task_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
    }

    pub fn on_tool_start(
        &self,
        tool_name: &str,
        query: &str,
        session_id: &str,
        agent_id: &str,
    ) -> TaskContext {
        catch_unwind(AssertUnwindSafe(|| {
            self.on_tool_start_inner(tool_name, query, session_id, agent_id)
        }))
        .unwrap_or_else(|_| TaskContext {
            task_id: String::new(),
            session_id: session_id.to_owned(),
            triage_class: String::new(),
            profile_intent: String::new(),
            profile_complexity: String::new(),
            filtered_lines: 0,
            start_time: Instant::now(),
        })
    }

    fn on_tool_start_inner(
        &self,
        tool_name: &str,
        query: &str,
        session_id: &str,
        agent_id: &str,
    ) -> TaskContext {
        let profile = self
            .triage
            .analyze(&TaskAnalysisInput {
                query: format!("{tool_name}: {query}"),
                ..Default::default()
            })
            .map(|hypothesis| hypothesis.profile)
            .unwrap_or_default();
        self.remember_profile(session_id, profile.clone());
        let mut envelope = TaskSpine::create_envelope(query, session_id, agent_id);
        TaskSpine::enrich_from_triage(&mut envelope, &protocol_profile(&profile));
        TaskContext {
            task_id: envelope.task_id.as_str().to_owned(),
            session_id: session_id.to_owned(),
            triage_class: profile.task_class,
            profile_intent: profile.intent,
            profile_complexity: profile.complexity,
            filtered_lines: 0,
            start_time: Instant::now(),
        }
    }

    fn remember_profile(
        &self,
        session_id: &str,
        profile: crate::core::triage::profile::TaskProfileLocal,
    ) {
        const MAX_SESSION_PROFILES: usize = 128;
        let mut profiles = self
            .task_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !profiles.contains_key(session_id) && profiles.len() >= MAX_SESSION_PROFILES {
            profiles.clear();
        }
        profiles.insert(session_id.to_owned(), profile);
    }

    pub fn on_tool_end(
        &self,
        ctx: &TaskContext,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
        success: bool,
    ) -> Option<crate::core::value_gate::ValueAssessment> {
        catch_unwind(AssertUnwindSafe(|| {
            self.on_tool_end_inner(ctx, input_tokens, output_tokens, model, success)
        }))
        .ok()
    }

    /// Records a completed tool and schedules an accepted Shadow Mode sample.
    ///
    /// Shadow work is detached from the MCP response path: inability to spawn
    /// or persist a comparison must never affect the completed tool call.
    pub fn on_tool_end_with_shadow(
        &self,
        ctx: &TaskContext,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
        success: bool,
        shadow_auto_record: bool,
        shadow_tokens: Option<(u64, u64)>,
    ) -> Option<crate::core::value_gate::ValueAssessment> {
        let result = self.on_tool_end(ctx, input_tokens, output_tokens, model, success);
        let Some(assessment) = result.as_ref() else {
            return result;
        };
        let (raw_input_tokens, compressed_input_tokens) =
            shadow_tokens.unwrap_or((output_tokens, output_tokens));
        let entry = crate::core::live_evidence_ledger::EvidenceLedgerEntry::completed(
            &ctx.task_id,
            &ctx.session_id,
            &ctx.triage_class,
            raw_input_tokens.saturating_sub(compressed_input_tokens),
            compressed_input_tokens,
            assessment.cpao_micros,
            assessment.outcome_accepted,
        );
        if let Err(error) = crate::core::live_evidence_ledger::append_completion(&entry) {
            tracing::warn!("failed to persist evidence ledger entry: {error}");
        }
        if !shadow_auto_record || !assessment.outcome_accepted {
            return result;
        }

        let duration_ms = u64::try_from(ctx.start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
        let task = crate::core::shadow::ShadowTask {
            task_id: assessment.task_id.clone(),
            query: format!("{}: {}", ctx.profile_intent, ctx.profile_complexity),
            raw_input_tokens,
            compressed_input_tokens,
            output_tokens,
            model_used: assessment.model.clone(),
            outcome_signals: vec![OutcomeSignal::BuildSucceeded],
            duration_ms,
        };
        let _ = std::thread::Builder::new()
            .name("lean-ctx-shadow".into())
            .spawn(move || {
                crate::core::shadow::runtime::ShadowRuntime::on_task_complete(&task);
            });
        result
    }

    fn on_tool_end_inner(
        &self,
        ctx: &TaskContext,
        input_tokens: u64,
        output_tokens: u64,
        model: &str,
        success: bool,
    ) -> crate::core::value_gate::ValueAssessment {
        let cost = ExecutionCost {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            model: model.to_owned(),
            provider: "mcp".to_owned(),
            estimated_cost_micros: calculate_cost(input_tokens, output_tokens, 0, model),
        };
        let outcome = TaskOutcome {
            task_id: ctx.task_id.clone(),
            completed: true,
            signals: vec![if success {
                OutcomeSignal::BuildSucceeded
            } else {
                OutcomeSignal::CompileError
            }],
        };
        let assessment = ValueGate::evaluate_task(&ctx.task_id, &cost, &outcome);
        self.value_gate_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(&assessment);
        assessment
    }

    #[cfg(test)]
    pub(crate) fn with_triage(triage: TriageEngine) -> Self {
        Self {
            triage,
            value_gate_store: Arc::new(Mutex::new(ValueGateStore::default())),
            task_profiles: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn latest_assessment_accepted(&self) -> Option<bool> {
        self.value_gate_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recent(1)
            .first()
            .map(|assessment| assessment.outcome_accepted)
    }

    #[cfg(test)]
    pub(crate) fn assessment_for(
        &self,
        task_id: &str,
    ) -> Option<crate::core::value_gate::ValueAssessment> {
        self.value_gate_store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recent(100)
            .into_iter()
            .find(|assessment| assessment.task_id == task_id)
    }
}
