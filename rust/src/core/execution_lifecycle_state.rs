// SPDX-License-Identifier: Apache-2.0
//! Task admission and ordered lifecycle state transitions.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};

use super::{
    DecisionSpine, DispatchState, LIFECYCLE_STAGE_ORDER, LifecycleOutcome, LifecycleRunError,
    LifecycleStage, ProductEntitlements, RunState, RuntimeContext, SharedRun, StageDisposition,
    StageExecution, TaskContext, ToolRequest, ToolSurface, task_class,
};
use crate::core::{
    decision_loop::protocol_profile,
    outcome::contracts::TaskClass,
    task_spine::TaskSpine,
    triage::{TaskAnalysisInput, TriageEngine, profile::TaskProfileLocal},
    value_gate::{ValueAssessment, ValueGate},
};

impl TaskContext {
    #[cfg(test)]
    pub(crate) fn autopilot_planning_attempts(&self) -> &std::sync::atomic::AtomicUsize {
        &self.shared.autopilot_planning_attempts
    }

    pub(crate) fn planning_query(&self) -> Option<&str> {
        self.planning_query.as_deref()
    }

    /// Canonical planning evidence; this alone is not execution/acceptance evidence.
    pub fn autopilot_decision(
        &self,
    ) -> Option<&crate::core::context_kernel::autopilot::TaskAutopilotDecision> {
        self.shared
            .autopilot
            .get()
            .map(|prepared| prepared.decision())
    }

    pub(crate) fn attach_autopilot(
        &self,
        prepared: Arc<crate::core::context_kernel::bridge::runtime::PreparedKernelContext>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            prepared.decision().context_projection().task_id == self.envelope.task_id
                && self.task_id == self.envelope.task_id.as_str(),
            "autopilot task lineage mismatch"
        );
        anyhow::ensure!(
            self.completed_stages().last() == Some(&LifecycleStage::GatherContextStrategy),
            "autopilot may only be attached before dispatch"
        );
        self.shared
            .autopilot
            .set(prepared)
            .map_err(|_| anyhow::anyhow!("autopilot decision already attached"))
    }

    pub(crate) fn autopilot_task_class(&self) -> TaskClass {
        task_class(&self.profile_intent)
    }

    pub(super) async fn cached_or_claim_dispatch<T, E>(
        &self,
    ) -> Option<Result<T, LifecycleRunError<E>>>
    where
        T: Clone + Send + Sync + 'static,
        E: Clone + Send + Sync + 'static,
    {
        loop {
            let notified = self.shared.dispatch_completed.notified();
            {
                let mut state = self
                    .shared
                    .dispatch
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &*state {
                    DispatchState::Finished(cached) => {
                        return Some(
                            cached
                                .downcast_ref::<Result<T, LifecycleRunError<E>>>()
                                .cloned()
                                .unwrap_or(Err(LifecycleRunError::ReplayTypeMismatch)),
                        );
                    }
                    DispatchState::Aborted(outcome) => {
                        return Some(Err(LifecycleRunError::Aborted(outcome.clone())));
                    }
                    DispatchState::Idle => {
                        if let RunState::Completed(outcome) = &*self
                            .shared
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                        {
                            return Some(Err(LifecycleRunError::Aborted(outcome.clone())));
                        }
                        *state = DispatchState::Running;
                        return None;
                    }
                    DispatchState::Running => {}
                }
            }
            notified.await;
        }
    }

    pub fn advance(&self, stage: LifecycleStage) -> bool {
        self.record_stage(stage, StageDisposition::Applied)
    }

    pub fn skip(&self, stage: LifecycleStage, reason: &'static str) -> bool {
        self.record_stage(stage, StageDisposition::Skipped(reason))
    }

    pub(super) fn record_stage(
        &self,
        stage: LifecycleStage,
        disposition: StageDisposition,
    ) -> bool {
        let mut stages = self
            .shared
            .stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if stages.last().is_some_and(|entry| entry.stage == stage) {
            return true;
        }
        let Some(expected) = LIFECYCLE_STAGE_ORDER.get(stages.len()) else {
            return false;
        };
        if *expected != stage {
            return false;
        }
        stages.push(StageExecution { stage, disposition });
        true
    }

    pub(super) fn skip_through(&self, target: LifecycleStage, reason: &'static str) {
        let Some(target_index) = LIFECYCLE_STAGE_ORDER
            .iter()
            .position(|stage| *stage == target)
        else {
            return;
        };
        if self
            .shared
            .stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
            > target_index
        {
            return;
        }
        loop {
            let next = {
                let stages = self
                    .shared
                    .stages
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                LIFECYCLE_STAGE_ORDER.get(stages.len()).copied()
            };
            let Some(next) = next else { return };
            let _ = self.skip(next, reason);
            if next == target {
                return;
            }
        }
    }

    pub fn advance_through(&self, target: LifecycleStage) {
        let Some(target_index) = LIFECYCLE_STAGE_ORDER
            .iter()
            .position(|stage| *stage == target)
        else {
            return;
        };
        let completed = self
            .shared
            .stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        if completed > target_index {
            return;
        }
        loop {
            let next = {
                let stages = self
                    .shared
                    .stages
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                LIFECYCLE_STAGE_ORDER.get(stages.len()).copied()
            };
            let Some(next) = next else { return };
            let _ = self.advance(next);
            if next == target {
                return;
            }
        }
    }

    pub fn completed_stages(&self) -> Vec<LifecycleStage> {
        self.shared
            .stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|entry| entry.stage)
            .collect()
    }

    pub fn stage_executions(&self) -> Vec<StageExecution> {
        self.shared
            .stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn outcome(&self) -> Option<LifecycleOutcome> {
        match &*self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            RunState::Completed(outcome) => Some(outcome.as_ref().clone()),
            RunState::Open | RunState::Completing => None,
        }
    }
}

/// Field identity is structural: caller-controlled delimiters and absent values
/// cannot alias another caller's replay scope. This map is process-local, so no
/// persisted key migration or second replay store is needed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct TaskReplayKey {
    surface: ToolSurface,
    session_id: String,
    agent_id: String,
    tool_name: String,
    query: Option<String>,
    client_name: Option<String>,
    project_root: Option<String>,
    task_identity: Option<crate::core::task_spine::AdmittedTaskIdentity>,
    idempotency_key: String,
}

impl DecisionSpine {
    pub(super) fn new(triage: TriageEngine, value_gate: ValueGate) -> Self {
        Self {
            triage,
            value_gate,
            task_profiles: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()),
            assessments: Mutex::new(HashMap::new()),
            outcomes: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn begin(
        &self,
        request: ToolRequest,
        runtime: RuntimeContext,
        entitlements: ProductEntitlements,
        identity: Option<&crate::core::task_spine::AdmittedTaskIdentity>,
    ) -> TaskContext {
        let run_key = request.idempotency_key.as_ref().map(|key| TaskReplayKey {
            surface: request.surface,
            session_id: request.session_id.clone(),
            agent_id: request.agent_id.clone(),
            tool_name: request.tool_name.clone(),
            query: request.query.clone(),
            client_name: runtime.client_name.clone(),
            project_root: runtime.project_root.clone(),
            task_identity: identity.cloned(),
            idempotency_key: key.clone(),
        });
        if let Some(key) = run_key.as_ref()
            && let Some(existing) = self
                .runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(key)
                .cloned()
        {
            TaskSpine::set(existing.envelope.clone());
            return existing;
        }

        let query = request.query.as_deref().unwrap_or(&request.tool_name);
        let triage_query = match request.query.as_deref() {
            Some(query) if request.tool_name == "decision_loop" => query.to_owned(),
            Some(query) => format!("{}: {query}", request.tool_name),
            None => String::new(),
        };
        let profile = self
            .triage
            .analyze(&TaskAnalysisInput {
                query: triage_query,
                ..Default::default()
            })
            .map(|hypothesis| hypothesis.profile)
            .unwrap_or_default();
        self.remember_profile(&request.session_id, profile.clone());
        let mut envelope = TaskSpine::create_envelope_with_identity(
            query,
            &request.session_id,
            &request.agent_id,
            runtime.project_root.as_deref(),
            identity,
        );
        TaskSpine::enrich_from_triage(&mut envelope, &protocol_profile(&profile));
        let context = TaskContext {
            task_id: envelope.task_id.as_str().to_owned(),
            session_id: request.session_id,
            triage_class: profile.task_class.clone(),
            profile_intent: profile.intent.clone(),
            profile_complexity: profile.complexity.clone(),
            filtered_lines: 0,
            start_time: Instant::now(),
            envelope,
            profile,
            surface: request.surface,
            runtime,
            entitlements,
            planning_query: request.query,
            shared: Arc::new(SharedRun::default()),
        };
        if let Some(key) = run_key {
            let mut runs = self
                .runs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match runs.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let existing = entry.get().clone();
                    TaskSpine::set(existing.envelope.clone());
                    return existing;
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(context.clone());
                }
            }
        }
        context
    }

    pub(super) fn remember_profile(&self, session_id: &str, profile: TaskProfileLocal) {
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

    pub(super) fn profile_for_session(&self, session_id: &str) -> Option<TaskProfileLocal> {
        self.task_profiles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .cloned()
    }

    pub(super) fn assessment_for(&self, task_id: &str) -> Option<ValueAssessment> {
        self.assessments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(task_id)
            .cloned()
    }
}
