// SPDX-License-Identifier: Apache-2.0
//! Canonical execution lifecycle and subordinate Decision Spine state.

use std::{
    any::Any,
    cell::RefCell,
    collections::HashMap,
    fmt,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::Instant,
};

tokio::task_local! {
    static DEFERRED_HEATMAP: RefCell<Vec<HeatmapObservation>>;
}

thread_local! {
    static SYNC_DEFERRED_HEATMAP: RefCell<Option<Vec<HeatmapObservation>>> = const {
        RefCell::new(None)
    };
}

use lean_ctx_protocol::{AcceptanceState, AcceptedOutcomeV1, TaskEnvelopeV1};

use crate::core::{
    outcome::{
        contracts::{OutcomeContractV1, TaskClass},
        evaluator::OutcomeEvaluator,
        signals::{LocalSignalAdapters, OutcomeSignal as CanonicalSignal},
    },
    task_spine::TaskSpine,
    triage::{TriageEngine, profile::TaskProfileLocal},
    value_gate::{
        ExecutionCost, OutcomeSignal, TaskOutcome, ValueAssessment, ValueGate,
        cost_tracker::calculate_cost,
    },
};

/// Ordered responsibilities owned by every execution lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStage {
    IdentifyCaller,
    ResolveWorkspace,
    ResolveEntitlements,
    ApplySecurityBoundaries,
    GatherContextStrategy,
    AskAutopilot,
    DispatchPrimitive,
    ReversiblePostProcess,
    RecordContextIr,
    RecordLedger,
    RecordEvidence,
    RecordOutcome,
    UpdateLearning,
    UpdateHeatmap,
    UpdateBreakEven,
    Checkpoint,
    ScheduleTelemetry,
    FlushState,
}

pub const LIFECYCLE_STAGE_ORDER: [LifecycleStage; 18] = [
    LifecycleStage::IdentifyCaller,
    LifecycleStage::ResolveWorkspace,
    LifecycleStage::ResolveEntitlements,
    LifecycleStage::ApplySecurityBoundaries,
    LifecycleStage::GatherContextStrategy,
    LifecycleStage::AskAutopilot,
    LifecycleStage::DispatchPrimitive,
    LifecycleStage::ReversiblePostProcess,
    LifecycleStage::RecordContextIr,
    LifecycleStage::RecordLedger,
    LifecycleStage::RecordEvidence,
    LifecycleStage::RecordOutcome,
    LifecycleStage::UpdateLearning,
    LifecycleStage::UpdateHeatmap,
    LifecycleStage::UpdateBreakEven,
    LifecycleStage::Checkpoint,
    LifecycleStage::ScheduleTelemetry,
    LifecycleStage::FlushState,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageDisposition {
    Applied,
    Skipped(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageExecution {
    pub stage: LifecycleStage,
    pub disposition: StageDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolSurface {
    Mcp,
    Cli,
    Daemon,
    Hook,
    Proxy,
}

#[derive(Debug, Clone)]
pub struct ToolRequest {
    pub tool_name: String,
    pub query: Option<String>,
    pub session_id: String,
    pub agent_id: String,
    pub surface: ToolSurface,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeContext {
    pub client_name: Option<String>,
    pub project_root: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProductEntitlements {
    pub autopilot: bool,
    pub personalized_learning: bool,
}

#[derive(Debug, Clone)]
pub struct CompletionObservation {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub provider: String,
    pub success: bool,
    /// Evidence-bearing legacy signals. An empty set never implies acceptance.
    pub outcome_signals: Vec<OutcomeSignal>,
    pub shadow_auto_record: bool,
    pub shadow_tokens: Option<(u64, u64)>,
    pub proxy_economics: Option<ProxyEconomicsObservation>,
    pub heatmap: Option<HeatmapObservation>,
}

#[derive(Debug, Clone)]
pub struct ProxyEconomicsObservation {
    pub tokens_pruned: usize,
    pub original_tokens: usize,
    pub task_class: String,
}

#[derive(Debug, Clone)]
pub struct HeatmapObservation {
    pub path: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
}

/// Defer heatmap writes to stage 14 when called inside an async lifecycle run.
pub(crate) fn record_heatmap_access(path: &str, original_tokens: usize, saved_tokens: usize) {
    let observation = HeatmapObservation {
        path: path.to_owned(),
        original_tokens,
        saved_tokens,
    };
    if DEFERRED_HEATMAP
        .try_with(|pending| pending.borrow_mut().push(observation.clone()))
        .is_ok()
    {
        return;
    }
    let deferred = SYNC_DEFERRED_HEATMAP.with(|pending| {
        let mut pending = pending.borrow_mut();
        pending.as_mut().is_some_and(|items| {
            items.push(observation);
            true
        })
    });
    if !deferred {
        crate::core::heatmap::record_file_access(path, original_tokens, saved_tokens);
    }
}

pub(crate) fn begin_sync_heatmap_capture() {
    SYNC_DEFERRED_HEATMAP.with(|pending| *pending.borrow_mut() = Some(Vec::new()));
}

fn take_deferred_heatmap() -> Vec<HeatmapObservation> {
    DEFERRED_HEATMAP
        .try_with(|pending| std::mem::take(&mut *pending.borrow_mut()))
        .unwrap_or_else(|_| {
            SYNC_DEFERRED_HEATMAP.with(|pending| pending.borrow_mut().take().unwrap_or_default())
        })
}

/// Owned cancellation state, finalized synchronously by the canonical guard.
pub struct CancellationCompletion {
    pub observation: CompletionObservation,
    pub ledger: StageDisposition,
}

pub type CancellationHandler = Box<dyn FnOnce() -> CancellationCompletion + Send>;

#[async_trait::async_trait]
pub trait ExecutionDriver: Send {
    type Primitive: Send;
    type Processed: Send;
    type Output: Clone + Send + Sync + 'static;
    type Error: Clone + Send + Sync + 'static;

    /// Retain only observations needed if the in-flight driver future is dropped.
    /// The handler and normal ledger stage must share an exactly-once owner.
    fn cancellation_handler(&self) -> Option<CancellationHandler> {
        None
    }

    async fn identify_caller(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("caller identity unavailable"))
    }

    async fn resolve_workspace(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("workspace unavailable"))
    }

    async fn resolve_entitlements(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("no product entitlement source"))
    }

    async fn apply_security_boundaries(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("no surface security adapter"))
    }

    async fn gather_context_strategy(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("no context strategy"))
    }

    async fn ask_autopilot(
        &mut self,
        context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(if context.entitlements.autopilot {
            // Entitlement is permission, not evidence that a planner ran.
            StageDisposition::Skipped("no canonical autopilot decision")
        } else {
            StageDisposition::Skipped("not entitled")
        })
    }

    /// Re-run mutable admission policy before returning an idempotent cached result.
    async fn validate_cached_replay(
        &mut self,
        _context: &TaskContext,
    ) -> Result<Option<Self::Output>, Self::Error> {
        Ok(None)
    }

    async fn dispatch_primitive(
        &mut self,
        context: &TaskContext,
    ) -> Result<(Self::Primitive, StageDisposition), Self::Error>;

    async fn reversible_post_process(
        &mut self,
        context: &TaskContext,
        primitive: Self::Primitive,
    ) -> Result<(Self::Processed, StageDisposition), Self::Error>;

    async fn record_context_ir(
        &mut self,
        context: &TaskContext,
        processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error>;

    async fn record_ledger(
        &mut self,
        context: &TaskContext,
        processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error>;

    async fn checkpoint(
        &mut self,
        _context: &TaskContext,
        _output: &mut Self::Output,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Skipped("checkpoint not configured"))
    }

    fn output_from_processed(&mut self, processed: Self::Processed) -> Self::Output;

    fn observe(
        &self,
        result: &Result<Self::Output, LifecycleRunError<Self::Error>>,
    ) -> CompletionObservation;
}

impl CompletionObservation {
    pub fn tool_result(input_tokens: u64, output_tokens: u64, model: &str, success: bool) -> Self {
        Self {
            input_tokens,
            output_tokens,
            model: model.to_owned(),
            provider: "mcp".to_owned(),
            success,
            // Transport status does not establish compiler or task-quality evidence.
            outcome_signals: Vec::new(),
            shadow_auto_record: false,
            shadow_tokens: None,
            proxy_economics: None,
            heatmap: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LifecycleOutcome {
    pub accepted_outcome: AcceptedOutcomeV1,
    pub assessment: Option<ValueAssessment>,
}

#[derive(Debug, Clone)]
enum RunState {
    Open,
    Completing,
    Completed(Box<LifecycleOutcome>),
}

#[cfg(test)]
type FinalizationHook = Arc<dyn Fn(LifecycleStage, Option<&LifecycleOutcome>) + Send + Sync>;

#[cfg(test)]
fn test_finalization_point(
    context: &TaskContext,
    stage: LifecycleStage,
    outcome: Option<&LifecycleOutcome>,
) {
    let hook = context
        .shared
        .finalization_hook
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(hook) = hook {
        hook(stage, outcome);
    }
}

struct SharedRun {
    state: Mutex<RunState>,
    completed: Condvar,
    dispatch: Mutex<DispatchState>,
    dispatch_completed: tokio::sync::Notify,
    stages: Mutex<Vec<StageExecution>>,
    computed_outcome: OnceLock<LifecycleOutcome>,
    telemetry_started: std::sync::atomic::AtomicBool,
    flush_started: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    flush_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    finalization_hook: Mutex<Option<FinalizationHook>>,
    autopilot: OnceLock<Arc<crate::core::context_kernel::bridge::runtime::PreparedKernelContext>>,
    #[cfg(test)]
    autopilot_planning_attempts: std::sync::atomic::AtomicUsize,
}

enum DispatchState {
    Idle,
    Running,
    Finished(Arc<dyn Any + Send + Sync>),
    Aborted(Box<LifecycleOutcome>),
}

#[derive(Debug, Clone)]
pub enum LifecycleRunError<E> {
    Dispatch(E),
    Aborted(Box<LifecycleOutcome>),
    /// A cached execution belongs to a different typed driver contract.
    ReplayTypeMismatch,
}

impl fmt::Debug for SharedRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SharedRun").finish_non_exhaustive()
    }
}

impl Default for SharedRun {
    fn default() -> Self {
        Self {
            state: Mutex::new(RunState::Open),
            completed: Condvar::new(),
            dispatch: Mutex::new(DispatchState::Idle),
            dispatch_completed: tokio::sync::Notify::new(),
            stages: Mutex::new(Vec::new()),
            computed_outcome: OnceLock::new(),
            telemetry_started: std::sync::atomic::AtomicBool::new(false),
            flush_started: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            flush_hook: Mutex::new(None),
            #[cfg(test)]
            finalization_hook: Mutex::new(None),
            autopilot: OnceLock::new(),
            #[cfg(test)]
            autopilot_planning_attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// Handle shared by surface adapters for one idempotent execution.
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub task_id: String,
    pub session_id: String,
    pub triage_class: String,
    pub profile_intent: String,
    pub profile_complexity: String,
    pub filtered_lines: usize,
    pub start_time: Instant,
    pub envelope: TaskEnvelopeV1,
    pub profile: TaskProfileLocal,
    pub surface: ToolSurface,
    pub runtime: RuntimeContext,
    pub entitlements: ProductEntitlements,
    planning_query: Option<String>,
    shared: Arc<SharedRun>,
}

#[path = "execution_lifecycle_state.rs"]
mod task_state;

#[derive(Debug)]
struct DecisionSpine {
    triage: TriageEngine,
    value_gate: ValueGate,
    task_profiles: Mutex<HashMap<String, TaskProfileLocal>>,
    runs: Mutex<HashMap<task_state::TaskReplayKey, TaskContext>>,
    assessments: Mutex<HashMap<String, ValueAssessment>>,
    outcomes: Mutex<HashMap<String, LifecycleOutcome>>,
}

/// Sole owner of task admission, ordered completion, and exact-once side effects.
#[derive(Debug, Clone)]
pub struct ExecutionLifecycle {
    spine: Arc<DecisionSpine>,
}

impl Default for ExecutionLifecycle {
    fn default() -> Self {
        Self::with_triage(TriageEngine::default())
    }
}

impl ExecutionLifecycle {
    pub fn global() -> &'static Self {
        static LIFECYCLE: std::sync::OnceLock<ExecutionLifecycle> = std::sync::OnceLock::new();
        LIFECYCLE.get_or_init(Self::default)
    }

    pub fn with_triage(triage: TriageEngine) -> Self {
        Self::with_components(triage, ValueGate)
    }

    pub fn with_components(triage: TriageEngine, value_gate: ValueGate) -> Self {
        Self {
            spine: Arc::new(DecisionSpine::new(triage, value_gate)),
        }
    }

    pub fn begin(
        &self,
        request: ToolRequest,
        runtime: RuntimeContext,
        entitlements: ProductEntitlements,
    ) -> TaskContext {
        self.spine.begin(request, runtime, entitlements, None)
    }

    fn prepare(&self, context: &TaskContext) {
        let _ = context.advance(LifecycleStage::IdentifyCaller);
        let _ = context.advance(LifecycleStage::ResolveWorkspace);
        let _ = context.advance(LifecycleStage::ResolveEntitlements);
        let _ = context.advance(LifecycleStage::ApplySecurityBoundaries);
        let _ = context.advance(LifecycleStage::GatherContextStrategy);
        if context.entitlements.autopilot {
            let _ = context.skip(
                LifecycleStage::AskAutopilot,
                "no canonical autopilot decision",
            );
        } else {
            let _ = context.skip(LifecycleStage::AskAutopilot, "not entitled");
        }
    }

    pub async fn run<D>(
        &self,
        request: ToolRequest,
        runtime: RuntimeContext,
        entitlements: ProductEntitlements,
        driver: D,
    ) -> Result<D::Output, LifecycleRunError<D::Error>>
    where
        D: ExecutionDriver,
    {
        self.run_with_task_identity(request, runtime, entitlements, driver, None)
            .await
    }

    /// Preserve the sole lifecycle owner while accepting an explicit host identity.
    pub(crate) async fn run_with_task_identity<D>(
        &self,
        request: ToolRequest,
        runtime: RuntimeContext,
        entitlements: ProductEntitlements,
        driver: D,
        identity: Option<crate::core::task_spine::AdmittedTaskIdentity>,
    ) -> Result<D::Output, LifecycleRunError<D::Error>>
    where
        D: ExecutionDriver,
    {
        // Begin must not leak thread-local identity onto a reusable async worker.
        TaskSpine::scope(
            None,
            DEFERRED_HEATMAP.scope(
                RefCell::new(Vec::new()),
                self.run_scoped(request, runtime, entitlements, driver, identity),
            ),
        )
        .await
    }

    async fn run_scoped<D>(
        &self,
        request: ToolRequest,
        runtime: RuntimeContext,
        entitlements: ProductEntitlements,
        driver: D,
        identity: Option<crate::core::task_spine::AdmittedTaskIdentity>,
    ) -> Result<D::Output, LifecycleRunError<D::Error>>
    where
        D: ExecutionDriver,
    {
        let context = self
            .spine
            .begin(request, runtime, entitlements, identity.as_ref());
        TaskSpine::scope(
            Some(context.envelope.clone()),
            // Keep the complete driver state out of every outer transport future.
            Box::pin(self.run_context(context, driver)),
        )
        .await
    }

    async fn run_context<D>(
        &self,
        context: TaskContext,
        mut driver: D,
    ) -> Result<D::Output, LifecycleRunError<D::Error>>
    where
        D: ExecutionDriver,
    {
        if let Some(cached) = context
            .cached_or_claim_dispatch::<D::Output, D::Error>()
            .await
        {
            if matches!(cached, Err(LifecycleRunError::ReplayTypeMismatch)) {
                return cached;
            }
            return match driver.validate_cached_replay(&context).await {
                Ok(Some(current_denial)) => Ok(current_denial),
                Ok(None) => cached,
                Err(error) => Err(LifecycleRunError::Dispatch(error)),
            };
        }
        let mut permit = DispatchPermit::new(context.shared.clone());
        let guard = LifecycleGuard::for_run(self, context.clone(), driver.cancellation_handler());
        *context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = RunState::Completing;
        macro_rules! run_admission_stage {
            ($method:ident, $stage:expr) => {
                match driver.$method(&context).await {
                    Ok(disposition) => {
                        let _ = context.record_stage($stage, disposition);
                    }
                    Err(error) => {
                        let _ = context.advance($stage);
                        context.skip_through(LifecycleStage::RecordLedger, "admission failed");
                        let result = Err(LifecycleRunError::Dispatch(error));
                        let _ = guard.complete(driver.observe(&result));
                        permit.publish(result.clone());
                        return result;
                    }
                }
            };
        }

        run_admission_stage!(identify_caller, LifecycleStage::IdentifyCaller);
        run_admission_stage!(resolve_workspace, LifecycleStage::ResolveWorkspace);
        run_admission_stage!(resolve_entitlements, LifecycleStage::ResolveEntitlements);
        run_admission_stage!(
            apply_security_boundaries,
            LifecycleStage::ApplySecurityBoundaries
        );
        run_admission_stage!(
            gather_context_strategy,
            LifecycleStage::GatherContextStrategy
        );
        run_admission_stage!(ask_autopilot, LifecycleStage::AskAutopilot);

        let primitive = match driver.dispatch_primitive(&context).await {
            Ok((primitive, disposition)) => {
                let _ = context.record_stage(LifecycleStage::DispatchPrimitive, disposition);
                primitive
            }
            Err(error) => {
                let _ = context.advance(LifecycleStage::DispatchPrimitive);
                let _ = context.skip(LifecycleStage::ReversiblePostProcess, "dispatch failed");
                let _ = context.skip(LifecycleStage::RecordContextIr, "dispatch failed");
                let _ = context.skip(LifecycleStage::RecordLedger, "dispatch failed");
                let result = Err(LifecycleRunError::Dispatch(error));
                let _ = guard.complete(driver.observe(&result));
                permit.publish(result.clone());
                return result;
            }
        };
        let processed = match driver.reversible_post_process(&context, primitive).await {
            Ok((processed, disposition)) => {
                let _ = context.record_stage(LifecycleStage::ReversiblePostProcess, disposition);
                processed
            }
            Err(error) => {
                let _ = context.advance(LifecycleStage::ReversiblePostProcess);
                let _ = context.skip(LifecycleStage::RecordContextIr, "post-processing failed");
                let _ = context.skip(LifecycleStage::RecordLedger, "post-processing failed");
                let result = Err(LifecycleRunError::Dispatch(error));
                let _ = guard.complete(driver.observe(&result));
                permit.publish(result.clone());
                return result;
            }
        };
        let ir = driver.record_context_ir(&context, &processed).await;
        match ir {
            Ok(disposition) => {
                let _ = context.record_stage(LifecycleStage::RecordContextIr, disposition);
            }
            Err(error) => {
                let _ = context.advance(LifecycleStage::RecordContextIr);
                let _ = context.skip(LifecycleStage::RecordLedger, "Context IR failed");
                let result = Err(LifecycleRunError::Dispatch(error));
                let _ = guard.complete(driver.observe(&result));
                permit.publish(result.clone());
                return result;
            }
        }
        match driver.record_ledger(&context, &processed).await {
            Ok(disposition) => {
                let _ = context.record_stage(LifecycleStage::RecordLedger, disposition);
            }
            Err(error) => {
                let _ = context.advance(LifecycleStage::RecordLedger);
                let result = Err(LifecycleRunError::Dispatch(error));
                let _ = guard.complete(driver.observe(&result));
                permit.publish(result.clone());
                return result;
            }
        }
        let mut output = driver.output_from_processed(processed);
        let provisional = Ok(output.clone());
        let observation = driver.observe(&provisional);
        *guard
            .observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(observation.clone());
        let fallback_observation = observation.clone();
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finalize_through_economics(&context, observation)
        })) {
            Ok(outcome) => {
                *guard
                    .outcome
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome.clone());
                match driver.checkpoint(&context, &mut output).await {
                    Ok(disposition) => {
                        let _ = context.record_stage(LifecycleStage::Checkpoint, disposition);
                    }
                    Err(_) => {
                        let _ = context.skip(LifecycleStage::Checkpoint, "checkpoint failed");
                    }
                }
                let telemetry_observation = driver.observe(&Ok(output.clone()));
                self.finish_cleanup(&context, &telemetry_observation);
                outcome
            }
            Err(_) => self.finalize_after_panic(&context, fallback_observation),
        };
        let mut state = context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = RunState::Completed(Box::new(outcome));
        context.shared.completed.notify_all();
        drop(state);
        let result = Ok(output);
        permit.publish(result.clone());
        result
    }

    pub fn complete(
        &self,
        context: &TaskContext,
        observation: CompletionObservation,
    ) -> LifecycleOutcome {
        self.prepare(context);
        self.complete_ordered(context, observation).0
    }

    /// Complete an open run and report whether this call owned finalization.
    ///
    /// Compatibility adapters use this to preserve their historic duplicate
    /// suppression while the lifecycle still returns a stable cached outcome
    /// through [`Self::complete`].
    pub fn complete_once(
        &self,
        context: &TaskContext,
        observation: CompletionObservation,
    ) -> Option<LifecycleOutcome> {
        self.prepare(context);
        let (outcome, completed_now) = self.complete_ordered(context, observation);
        completed_now.then_some(outcome)
    }

    fn complete_ordered(
        &self,
        context: &TaskContext,
        observation: CompletionObservation,
    ) -> (LifecycleOutcome, bool) {
        let mut state = context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match &*state {
                RunState::Completed(outcome) => return (outcome.as_ref().clone(), false),
                RunState::Completing => {
                    state = context
                        .shared
                        .completed
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                RunState::Open => {
                    *state = RunState::Completing;
                    break;
                }
            }
        }
        drop(state);
        context.skip_through(
            LifecycleStage::RecordLedger,
            "surface ended before dispatch",
        );

        let fallback_observation = observation.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finalize(context, observation)
        }))
        .unwrap_or_else(|_| {
            tracing::error!(
                task_id = context.task_id,
                "execution lifecycle finalizer panicked"
            );
            self.finalize_after_panic(context, fallback_observation)
        });
        let mut state = context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = RunState::Completed(Box::new(outcome.clone()));
        context.shared.completed.notify_all();
        (outcome, true)
    }

    fn complete_cancelled(
        &self,
        context: &TaskContext,
        observation: &CompletionObservation,
        completed_outcome: Option<LifecycleOutcome>,
    ) -> LifecycleOutcome {
        // A checkpoint can be cancelled after economics and the outcome are
        // recorded. Preserve that outcome and do not replay completed effects.
        let outcome = if let Some(outcome) = completed_outcome {
            outcome
        } else {
            // Keep cleanup outside outcome computation: a failed flush cannot
            // turn cancellation into fabricated task-quality rejection.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.finalize_through_economics(context, observation.clone())
            }))
            .unwrap_or_else(|_| self.finalize_after_panic(context, observation.clone()))
        };
        context.skip_through(LifecycleStage::Checkpoint, "execution cancelled");
        self.finish_cleanup(context, observation);
        self.spine
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(context.task_id.clone())
            .or_insert_with(|| outcome.clone());
        *context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            RunState::Completed(Box::new(outcome.clone()));
        context.shared.completed.notify_all();
        outcome
    }

    fn complete_owned(
        &self,
        context: &TaskContext,
        observation: CompletionObservation,
    ) -> LifecycleOutcome {
        context.skip_through(
            LifecycleStage::RecordLedger,
            "surface ended before dispatch",
        );
        let state = context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let RunState::Completed(outcome) = &*state {
            return outcome.as_ref().clone();
        }
        drop(state);
        let fallback_observation = observation.clone();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.finalize(context, observation)
        }))
        .unwrap_or_else(|_| self.finalize_after_panic(context, fallback_observation));
        let mut state = context
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = RunState::Completed(Box::new(outcome.clone()));
        context.shared.completed.notify_all();
        outcome
    }

    fn finalize(
        &self,
        context: &TaskContext,
        observation: CompletionObservation,
    ) -> LifecycleOutcome {
        let telemetry = observation.clone();
        let outcome = if context.completed_stages().len() >= 15 {
            self.outcome_for(&context.task_id)
                .unwrap_or_else(|| self.finalize_through_economics(context, observation))
        } else {
            self.finalize_through_economics(context, observation)
        };
        let _ = context.skip(LifecycleStage::Checkpoint, "checkpoint not configured");
        self.finish_cleanup(context, &telemetry);
        outcome
    }

    fn finalize_through_economics(
        &self,
        context: &TaskContext,
        observation: CompletionObservation,
    ) -> LifecycleOutcome {
        TaskSpine::set(context.envelope.clone());
        if let Some(outcome) = context.shared.computed_outcome.get() {
            // Never retry partially applied evidence, learning or economics.
            context.skip_through(
                LifecycleStage::UpdateBreakEven,
                "finalization already attempted",
            );
            return outcome.clone();
        }

        #[cfg(test)]
        test_finalization_point(context, LifecycleStage::RecordEvidence, None);

        let CompletionObservation {
            input_tokens,
            output_tokens,
            model,
            provider,
            success: _,
            outcome_signals,
            shadow_auto_record,
            shadow_tokens,
            proxy_economics,
            heatmap,
        } = observation;
        #[cfg(test)]
        let _ = (
            output_tokens,
            &outcome_signals,
            shadow_auto_record,
            shadow_tokens,
        );

        // Transport success/failure is not a quality signal or policy violation.
        let accepted_outcome =
            evaluate_outcome_signals(&context.task_id, &context.profile_intent, &outcome_signals);

        let assessment = (accepted_outcome.accepted != AcceptanceState::Unknown
            && !outcome_signals.is_empty())
        .then(|| {
            let cost = ExecutionCost {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                model: model.clone(),
                provider,
                estimated_cost_micros: calculate_cost(input_tokens, output_tokens, 0, &model),
            };
            let outcome = TaskOutcome {
                task_id: context.task_id.clone(),
                completed: true,
                signals: outcome_signals.clone(),
            };
            self.spine
                .value_gate
                .assess(&context.task_id, &cost, &outcome)
        });

        let outcome = LifecycleOutcome {
            accepted_outcome,
            assessment,
        };
        // Retain the evaluated result before any fallible persistence effects;
        // an evictable lookup map cannot be the sole authority during recovery.
        let _ = context.shared.computed_outcome.set(outcome.clone());

        if let Some(assessment) = outcome.assessment.as_ref() {
            #[cfg(not(test))]
            self.record_evidence(context, output_tokens, shadow_tokens, assessment);
            #[cfg(test)]
            let _ = assessment;
            let _ = context.advance(LifecycleStage::RecordEvidence);
        } else {
            let _ = context.skip(
                LifecycleStage::RecordEvidence,
                "outcome has no accepted evidence",
            );
        }
        if let Some(assessment) = outcome.assessment.as_ref() {
            self.spine
                .assessments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(context.task_id.clone(), assessment.clone());
        }
        self.spine
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(context.task_id.clone(), outcome.clone());
        let _ = context.advance(LifecycleStage::RecordOutcome);
        if let Some(assessment) = outcome.assessment.as_ref()
            && context.entitlements.personalized_learning
        {
            #[cfg(not(test))]
            crate::core::value_gate::record_assessment(assessment);
            #[cfg(not(test))]
            self.record_shadow(
                context,
                output_tokens,
                &outcome_signals,
                shadow_auto_record,
                shadow_tokens,
                assessment,
            );
            #[cfg(test)]
            let _ = assessment;
            let _ = context.advance(LifecycleStage::UpdateLearning);
        } else {
            let _ = context.skip(
                LifecycleStage::UpdateLearning,
                "not entitled or outcome unknown",
            );
        }
        let mut heatmaps = take_deferred_heatmap();
        if let Some(heatmap) = heatmap {
            heatmaps.push(heatmap);
        }
        if heatmaps.is_empty() {
            let _ = context.skip(LifecycleStage::UpdateHeatmap, "no file access recorded");
        } else {
            for heatmap in heatmaps {
                crate::core::heatmap::record_file_access(
                    &heatmap.path,
                    heatmap.original_tokens,
                    heatmap.saved_tokens,
                );
            }
            let _ = context.advance(LifecycleStage::UpdateHeatmap);
        }
        if context.surface == ToolSurface::Proxy {
            #[cfg(test)]
            test_finalization_point(context, LifecycleStage::UpdateBreakEven, Some(&outcome));
            let economics = proxy_economics.unwrap_or_else(|| ProxyEconomicsObservation {
                tokens_pruned: 0,
                original_tokens: usize::try_from(input_tokens).unwrap_or(usize::MAX),
                task_class: context.triage_class.clone(),
            });
            #[cfg(not(test))]
            crate::proxy::value_gate_proxy::record_completion(
                economics.tokens_pruned,
                economics.original_tokens,
                &economics.task_class,
                outcome.accepted_outcome.accepted,
            );
            #[cfg(test)]
            let _ = economics;
            let _ = context.advance(LifecycleStage::UpdateBreakEven);
        } else {
            let _ = context.skip(LifecycleStage::UpdateBreakEven, "no provider economics");
        }
        outcome
    }

    fn finish_cleanup(&self, context: &TaskContext, observation: &CompletionObservation) {
        if !context
            .completed_stages()
            .contains(&LifecycleStage::ScheduleTelemetry)
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.record_telemetry(context, observation);
            }))
            .is_err()
        {
            context.skip(LifecycleStage::ScheduleTelemetry, "telemetry panicked");
        }
        // Each effect reserves its attempt before writing. A partial failure
        // cannot unwind outcome publication or cause recovery to retry writes.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.flush_state(context);
        }));
    }

    fn record_telemetry(&self, context: &TaskContext, observation: &CompletionObservation) {
        if context
            .shared
            .telemetry_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            context.skip(
                LifecycleStage::ScheduleTelemetry,
                "telemetry already attempted",
            );
            return;
        }
        #[cfg(test)]
        test_finalization_point(context, LifecycleStage::ScheduleTelemetry, None);
        #[cfg(not(test))]
        {
            let metrics = crate::core::telemetry::global_metrics();
            metrics.record_tool_call(
                u64::try_from(context.start_time.elapsed().as_micros()).unwrap_or(u64::MAX),
                observation.success,
            );
            metrics.record_tokens(
                observation.input_tokens,
                observation.output_tokens,
                observation
                    .input_tokens
                    .saturating_sub(observation.output_tokens),
            );
        }
        #[cfg(test)]
        let _ = (context, observation);
        let _ = context.advance(LifecycleStage::ScheduleTelemetry);
    }

    fn flush_state(&self, context: &TaskContext) {
        // Reserve the effect, not only its stage marker: panic recovery and guard
        // drop must never retry a partially completed flush.
        if context
            .shared
            .flush_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[cfg(test)]
            {
                let hook = context
                    .shared
                    .flush_hook
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(hook) = hook {
                    hook();
                    return;
                }
            }
            crate::core::tool_lifecycle::flush_all();
        }));
        match result {
            Ok(()) => {
                let _ = context.advance(LifecycleStage::FlushState);
            }
            Err(payload) => {
                let _ = context.skip(LifecycleStage::FlushState, "state flush panicked");
                std::panic::resume_unwind(payload);
            }
        }
    }

    fn finalize_after_panic(
        &self,
        context: &TaskContext,
        mut observation: CompletionObservation,
    ) -> LifecycleOutcome {
        observation.success = false;
        // Infrastructure failure is not compiler or task-quality evidence.
        // Preserve a completed evaluation; otherwise quality remains unknown.
        let outcome = context
            .shared
            .computed_outcome
            .get()
            .cloned()
            .unwrap_or_else(|| {
                let contract =
                    OutcomeContractV1::for_task_class(task_class(&context.profile_intent));
                LifecycleOutcome {
                    accepted_outcome: OutcomeEvaluator::new()
                        .evaluate_with_context(
                            &contract,
                            &[],
                            crate::core::outcome::evaluator::EvaluationContext::new(
                                &context.task_id,
                                &contract.contract_version,
                                crate::core::outcome::evaluator::EVALUATOR_VERSION,
                            ),
                        )
                        .outcome,
                    assessment: None,
                }
            });
        context.skip_through(
            LifecycleStage::UpdateBreakEven,
            "lifecycle finalizer failed",
        );
        let _ = context.skip(LifecycleStage::Checkpoint, "lifecycle finalizer failed");
        self.finish_cleanup(context, &observation);
        if let Some(assessment) = outcome.assessment.as_ref() {
            self.spine
                .assessments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(context.task_id.clone(), assessment.clone());
        }
        self.spine
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(context.task_id.clone(), outcome.clone());
        outcome
    }

    #[cfg(not(test))]
    fn record_evidence(
        &self,
        context: &TaskContext,
        output_tokens: u64,
        shadow_tokens: Option<(u64, u64)>,
        assessment: &ValueAssessment,
    ) {
        let (raw_input_tokens, compressed_input_tokens) =
            shadow_tokens.unwrap_or((output_tokens, output_tokens));
        let entry = crate::core::live_evidence_ledger::EvidenceLedgerEntry::completed(
            &context.task_id,
            &context.session_id,
            &context.triage_class,
            raw_input_tokens.saturating_sub(compressed_input_tokens),
            compressed_input_tokens,
            assessment.cpao_micros,
            assessment.outcome_accepted,
        );
        if let Err(error) = crate::core::live_evidence_ledger::append_completion(&entry) {
            tracing::warn!("failed to persist evidence ledger entry: {error}");
        }
    }

    #[cfg(not(test))]
    fn record_shadow(
        &self,
        context: &TaskContext,
        output_tokens: u64,
        outcome_signals: &[OutcomeSignal],
        shadow_auto_record: bool,
        shadow_tokens: Option<(u64, u64)>,
        assessment: &ValueAssessment,
    ) {
        let (raw_input_tokens, compressed_input_tokens) =
            shadow_tokens.unwrap_or((output_tokens, output_tokens));
        if !shadow_auto_record || !assessment.outcome_accepted {
            return;
        }
        let duration_ms =
            u64::try_from(context.start_time.elapsed().as_millis()).unwrap_or(u64::MAX);
        let task = crate::core::shadow::ShadowTask {
            task_id: assessment.task_id.clone(),
            query: format!("{}: {}", context.profile_intent, context.profile_complexity),
            raw_input_tokens,
            compressed_input_tokens,
            output_tokens,
            model_used: assessment.model.clone(),
            outcome_signals: outcome_signals.to_vec(),
            duration_ms,
        };
        let _ = std::thread::Builder::new()
            .name("lean-ctx-shadow".into())
            .spawn(move || crate::core::shadow::runtime::ShadowRuntime::on_task_complete(&task));
    }

    pub fn profile_for_session(&self, session_id: &str) -> Option<TaskProfileLocal> {
        self.spine.profile_for_session(session_id)
    }

    pub fn assessment_for(&self, task_id: &str) -> Option<ValueAssessment> {
        self.spine.assessment_for(task_id)
    }

    pub fn outcome_for(&self, task_id: &str) -> Option<LifecycleOutcome> {
        self.spine
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(task_id)
            .cloned()
    }
}

/// RAII completion guard for error, timeout, and early-return paths.
pub struct LifecycleGuard<'a> {
    lifecycle: &'a ExecutionLifecycle,
    context: TaskContext,
    prepare_missing: bool,
    cancellation: Option<CancellationHandler>,
    observation: Mutex<Option<CompletionObservation>>,
    outcome: Mutex<Option<LifecycleOutcome>>,
}

struct DispatchPermit {
    shared: Arc<SharedRun>,
    published: bool,
}

impl DispatchPermit {
    fn new(shared: Arc<SharedRun>) -> Self {
        Self {
            shared,
            published: false,
        }
    }

    fn publish<T: Send + Sync + 'static>(&mut self, result: T) {
        *self
            .shared
            .dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            DispatchState::Finished(Arc::new(result));
        self.published = true;
        self.shared.dispatch_completed.notify_waiters();
    }
}

impl Drop for DispatchPermit {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let mut dispatch = self
            .shared
            .dispatch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let terminal = match &*self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            RunState::Completed(outcome) => Some(outcome.clone()),
            RunState::Open | RunState::Completing => None,
        };
        *dispatch = terminal.map_or(DispatchState::Idle, DispatchState::Aborted);
        self.shared.dispatch_completed.notify_waiters();
    }
}

impl<'a> LifecycleGuard<'a> {
    pub fn new(lifecycle: &'a ExecutionLifecycle, context: TaskContext) -> Self {
        lifecycle.prepare(&context);
        Self {
            lifecycle,
            context,
            prepare_missing: true,
            cancellation: None,
            observation: Mutex::new(None),
            outcome: Mutex::new(None),
        }
    }

    fn for_run(
        lifecycle: &'a ExecutionLifecycle,
        context: TaskContext,
        cancellation: Option<CancellationHandler>,
    ) -> Self {
        Self {
            lifecycle,
            context,
            prepare_missing: false,
            cancellation,
            observation: Mutex::new(None),
            outcome: Mutex::new(None),
        }
    }

    pub const fn context(&self) -> &TaskContext {
        &self.context
    }

    pub fn complete(&self, observation: CompletionObservation) -> LifecycleOutcome {
        if self.prepare_missing {
            self.lifecycle.complete(&self.context, observation)
        } else {
            self.lifecycle.complete_owned(&self.context, observation)
        }
    }
}

impl Drop for LifecycleGuard<'_> {
    fn drop(&mut self) {
        if self.context.outcome().is_some() {
            return;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut observation = CompletionObservation::tool_result(0, 0, "unknown", false);
            // Cancellation is not compiler or task-quality evidence.
            observation.outcome_signals.clear();
            if !self
                .context
                .completed_stages()
                .contains(&LifecycleStage::RecordLedger)
            {
                self.context.skip_through(
                    LifecycleStage::RecordContextIr,
                    "execution cancelled before stage completed",
                );
                let mut ledger = StageDisposition::Skipped("execution cancelled before ledger");
                if let Some(handler) = self.cancellation.take() {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler)) {
                        Ok(completion) => {
                            observation = completion.observation;
                            ledger = completion.ledger;
                        }
                        Err(_) => {
                            ledger = StageDisposition::Skipped("cancellation ledger panicked");
                        }
                    }
                }
                self.context
                    .record_stage(LifecycleStage::RecordLedger, ledger);
            }
            if let Some(saved) = self
                .observation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                observation = saved;
            }
            let completed_outcome = self
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let _ = if self.prepare_missing {
                self.lifecycle.complete(&self.context, observation)
            } else {
                self.lifecycle
                    .complete_cancelled(&self.context, &observation, completed_outcome)
            };
        }));
    }
}

/// Pure evaluation shared by real finalization and explicitly simulated proofs.
/// This function does not finalize a lifecycle or persist evidence.
pub(crate) fn evaluate_outcome_signals(
    task_id: &str,
    profile_intent: &str,
    signals: &[OutcomeSignal],
) -> AcceptedOutcomeV1 {
    let signals = canonical_signals(signals, task_id);
    let contract = OutcomeContractV1::for_task_class(task_class(profile_intent));
    let context = crate::core::outcome::evaluator::EvaluationContext::new(
        task_id,
        &contract.contract_version,
        crate::core::outcome::evaluator::EVALUATOR_VERSION,
    );
    OutcomeEvaluator::new()
        .evaluate_with_context(&contract, &signals, context)
        .outcome
}

fn task_class(value: &str) -> TaskClass {
    match value {
        "coding_fix" | "bug_fix" | "debugging" => TaskClass::BugFix,
        "refactor" | "coding_refactor" => TaskClass::Refactor,
        "test_addition" => TaskClass::TestAddition,
        "documentation" | "docs" => TaskClass::Documentation,
        _ => TaskClass::Investigation,
    }
}

fn canonical_signals(signals: &[OutcomeSignal], task_id: &str) -> Vec<CanonicalSignal> {
    signals
        .iter()
        .map(|signal| {
            let signal = match signal {
                OutcomeSignal::BuildSucceeded => LocalSignalAdapters::build_success(true),
                OutcomeSignal::TestsPassed => LocalSignalAdapters::tests_passing(true),
                OutcomeSignal::LintClean => LocalSignalAdapters::lint_clean(true),
                OutcomeSignal::UserAccepted => LocalSignalAdapters::human_acceptance(true),
                OutcomeSignal::UserRejected => LocalSignalAdapters::human_acceptance(false),
                OutcomeSignal::CompileError => LocalSignalAdapters::build_success(false),
                OutcomeSignal::TestFailed => LocalSignalAdapters::tests_passing(false),
            };
            signal.with_evidence_ref(format!("task:{task_id}"))
        })
        .collect()
}

#[cfg(test)]
#[path = "execution_lifecycle_tests.rs"]
mod tests;
