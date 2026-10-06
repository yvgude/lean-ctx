// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn host_task_scope_separates_replay_and_parent_lineage() {
    use crate::core::task_spine::AdmittedTaskIdentity;
    let lifecycle = ExecutionLifecycle::default();
    let session = format!("scope-{}", uuid::Uuid::new_v4());
    let first = AdmittedTaskIdentity {
        project_id: lean_ctx_protocol::ProjectId::new("portable-project").unwrap(),
        tenant_id: lean_ctx_protocol::TenantId::new("first-tenant").unwrap(),
    };
    let mut second = first.clone();
    second.tenant_id = lean_ctx_protocol::TenantId::new("second-tenant").unwrap();
    let request = |key: &str| ToolRequest {
        tool_name: "ctx_read".into(),
        query: None,
        session_id: session.clone(),
        agent_id: "same-agent".into(),
        surface: ToolSurface::Mcp,
        idempotency_key: Some(key.into()),
    };
    let runtime = RuntimeContext {
        client_name: None,
        project_root: Some("/actual/root".into()),
    };
    let begin = |identity, key| {
        lifecycle.spine.begin(
            request(key),
            runtime.clone(),
            ProductEntitlements::default(),
            identity,
        )
    };
    let a = begin(Some(&first), "same-key");
    let b = begin(Some(&second), "same-key");
    let legacy = begin(None, "same-key");
    assert_eq!(a.envelope.project_id, first.project_id);
    assert_eq!(a.envelope.tenant_id.as_ref(), Some(&first.tenant_id));
    assert_eq!(legacy.envelope.project_id.as_str(), "/actual/root");
    assert!(legacy.envelope.tenant_id.is_none());
    assert_ne!(a.task_id, b.task_id);
    assert_ne!(a.task_id, legacy.task_id);
    assert_ne!(a.envelope.trace_id, b.envelope.trace_id);
    assert!(a.envelope.parent_task_id.is_none());
    assert!(b.envelope.parent_task_id.is_none());
    assert_eq!(begin(Some(&first), "same-key").task_id, a.task_id);
    let child = begin(Some(&first), "next-key");
    assert_eq!(
        child.envelope.parent_task_id.as_ref(),
        Some(&a.envelope.task_id)
    );
    assert_eq!(child.envelope.trace_id, a.envelope.trace_id);
}

struct TestDriver {
    calls: Arc<Mutex<Vec<LifecycleStage>>>,
    context: Arc<Mutex<Option<TaskContext>>>,
    fail_at: Option<LifecycleStage>,
}

impl TestDriver {
    fn record(&self, stage: LifecycleStage) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(stage);
        if self.fail_at == Some(stage) {
            Err(format!("injected failure at {stage:?}"))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl ExecutionDriver for TestDriver {
    type Primitive = u8;
    type Processed = u8;
    type Output = u8;
    type Error = String;

    async fn identify_caller(
        &mut self,
        context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        *self.context.lock().unwrap() = Some(context.clone());
        self.record(LifecycleStage::IdentifyCaller)?;
        Ok(StageDisposition::Applied)
    }

    async fn resolve_workspace(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::ResolveWorkspace)?;
        Ok(StageDisposition::Applied)
    }

    async fn resolve_entitlements(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::ResolveEntitlements)?;
        Ok(StageDisposition::Applied)
    }

    async fn apply_security_boundaries(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::ApplySecurityBoundaries)?;
        Ok(StageDisposition::Applied)
    }

    async fn gather_context_strategy(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::GatherContextStrategy)?;
        Ok(StageDisposition::Applied)
    }

    async fn ask_autopilot(
        &mut self,
        _context: &TaskContext,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::AskAutopilot)?;
        Ok(StageDisposition::Skipped("not entitled"))
    }

    async fn dispatch_primitive(
        &mut self,
        context: &TaskContext,
    ) -> Result<(Self::Primitive, StageDisposition), Self::Error> {
        self.record(LifecycleStage::DispatchPrimitive)?;
        *self
            .context
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(context.clone());
        Ok((7, StageDisposition::Applied))
    }

    async fn reversible_post_process(
        &mut self,
        _context: &TaskContext,
        primitive: Self::Primitive,
    ) -> Result<(Self::Processed, StageDisposition), Self::Error> {
        self.record(LifecycleStage::ReversiblePostProcess)?;
        Ok((primitive, StageDisposition::Applied))
    }

    async fn record_context_ir(
        &mut self,
        _context: &TaskContext,
        _processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::RecordContextIr)?;
        Ok(StageDisposition::Applied)
    }

    async fn record_ledger(
        &mut self,
        _context: &TaskContext,
        _processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        self.record(LifecycleStage::RecordLedger)?;
        Ok(StageDisposition::Applied)
    }

    async fn checkpoint(
        &mut self,
        context: &TaskContext,
        output: &mut Self::Output,
    ) -> Result<StageDisposition, Self::Error> {
        assert_eq!(
            context.completed_stages(),
            LIFECYCLE_STAGE_ORDER[..15],
            "checkpoint must execute after economics"
        );
        self.record(LifecycleStage::Checkpoint)?;
        *output += 1;
        Ok(StageDisposition::Applied)
    }

    fn output_from_processed(&mut self, processed: Self::Processed) -> Self::Output {
        processed
    }

    fn observe(
        &self,
        result: &Result<Self::Output, LifecycleRunError<Self::Error>>,
    ) -> CompletionObservation {
        CompletionObservation::tool_result(1, 1, "test", result.is_ok())
    }
}

struct SlowDriver {
    dispatches: Arc<std::sync::atomic::AtomicUsize>,
    signals: Vec<OutcomeSignal>,
}

struct MismatchedReplayDriver {
    inner: TestDriver,
    validations: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl ExecutionDriver for MismatchedReplayDriver {
    type Primitive = u8;
    type Processed = u8;
    type Output = String;
    type Error = String;

    async fn validate_cached_replay(
        &mut self,
        _context: &TaskContext,
    ) -> Result<Option<Self::Output>, Self::Error> {
        self.validations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some("would mask the incompatible cached contract".into()))
    }

    async fn dispatch_primitive(
        &mut self,
        context: &TaskContext,
    ) -> Result<(Self::Primitive, StageDisposition), Self::Error> {
        self.inner.dispatch_primitive(context).await
    }

    async fn reversible_post_process(
        &mut self,
        context: &TaskContext,
        primitive: Self::Primitive,
    ) -> Result<(Self::Processed, StageDisposition), Self::Error> {
        self.inner.reversible_post_process(context, primitive).await
    }

    async fn record_context_ir(
        &mut self,
        context: &TaskContext,
        processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        self.inner.record_context_ir(context, processed).await
    }

    async fn record_ledger(
        &mut self,
        context: &TaskContext,
        processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        self.inner.record_ledger(context, processed).await
    }

    fn output_from_processed(&mut self, processed: Self::Processed) -> Self::Output {
        self.inner.output_from_processed(processed).to_string()
    }

    fn observe(
        &self,
        result: &Result<Self::Output, LifecycleRunError<Self::Error>>,
    ) -> CompletionObservation {
        CompletionObservation::tool_result(1, 1, "test", result.is_ok())
    }
}

struct BlockingCheckpointDriver {
    reached: Arc<tokio::sync::Notify>,
    cancellations: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl ExecutionDriver for SlowDriver {
    type Primitive = u8;
    type Processed = u8;
    type Output = u8;
    type Error = String;

    async fn dispatch_primitive(
        &mut self,
        _context: &TaskContext,
    ) -> Result<(Self::Primitive, StageDisposition), Self::Error> {
        self.dispatches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        Ok((7, StageDisposition::Applied))
    }

    async fn reversible_post_process(
        &mut self,
        _context: &TaskContext,
        primitive: Self::Primitive,
    ) -> Result<(Self::Processed, StageDisposition), Self::Error> {
        Ok((primitive, StageDisposition::Applied))
    }

    async fn record_context_ir(
        &mut self,
        _context: &TaskContext,
        _processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Applied)
    }

    async fn record_ledger(
        &mut self,
        _context: &TaskContext,
        _processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Applied)
    }

    fn output_from_processed(&mut self, processed: Self::Processed) -> Self::Output {
        processed
    }

    fn observe(
        &self,
        result: &Result<Self::Output, LifecycleRunError<Self::Error>>,
    ) -> CompletionObservation {
        let mut observation = CompletionObservation::tool_result(1, 1, "test", result.is_ok());
        if !self.signals.is_empty() {
            observation.outcome_signals.clone_from(&self.signals);
        }
        observation
    }
}

#[async_trait::async_trait]
impl ExecutionDriver for BlockingCheckpointDriver {
    type Primitive = u8;
    type Processed = u8;
    type Output = u8;
    type Error = String;

    fn cancellation_handler(&self) -> Option<CancellationHandler> {
        let calls = self.cancellations.clone();
        Some(Box::new(move || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("completed ledger must not be invoked again");
        }))
    }

    async fn dispatch_primitive(
        &mut self,
        _context: &TaskContext,
    ) -> Result<(Self::Primitive, StageDisposition), Self::Error> {
        Ok((7, StageDisposition::Applied))
    }

    async fn reversible_post_process(
        &mut self,
        _context: &TaskContext,
        primitive: Self::Primitive,
    ) -> Result<(Self::Processed, StageDisposition), Self::Error> {
        Ok((primitive, StageDisposition::Applied))
    }

    async fn record_context_ir(
        &mut self,
        _context: &TaskContext,
        _processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Applied)
    }

    async fn record_ledger(
        &mut self,
        _context: &TaskContext,
        _processed: &Self::Processed,
    ) -> Result<StageDisposition, Self::Error> {
        Ok(StageDisposition::Applied)
    }

    async fn checkpoint(
        &mut self,
        _context: &TaskContext,
        _output: &mut Self::Output,
    ) -> Result<StageDisposition, Self::Error> {
        self.reached.notify_one();
        std::future::pending().await
    }

    fn output_from_processed(&mut self, processed: Self::Processed) -> Self::Output {
        processed
    }

    fn observe(
        &self,
        result: &Result<Self::Output, LifecycleRunError<Self::Error>>,
    ) -> CompletionObservation {
        CompletionObservation::tool_result(1, 1, "test", result.is_ok())
    }
}

fn request(surface: ToolSurface, key: Option<&str>) -> ToolRequest {
    ToolRequest {
        tool_name: "synthetic".into(),
        query: Some("inspect result".into()),
        session_id: "lifecycle-test".into(),
        agent_id: "agent".into(),
        surface,
        idempotency_key: key.map(str::to_owned),
    }
}

#[tokio::test]
async fn run_invokes_owned_surface_stages_in_order() {
    let lifecycle = ExecutionLifecycle::default();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::new(Mutex::new(None));
    let result = lifecycle
        .run(
            request(ToolSurface::Mcp, None),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            TestDriver {
                calls: calls.clone(),
                context: captured.clone(),
                fail_at: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(result, 8);
    assert_eq!(
        *calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [
            LIFECYCLE_STAGE_ORDER[..10].as_ref(),
            &[LifecycleStage::Checkpoint],
        ]
        .concat()
    );
    let context = captured
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("driver captured context");
    assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
    assert_outcome_lineage(&context, &context.outcome().unwrap());
}

#[tokio::test]
async fn default_driver_does_not_treat_entitlement_as_a_decision() {
    let lifecycle = ExecutionLifecycle::default();
    let mut driver = SlowDriver {
        dispatches: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        signals: Vec::new(),
    };
    for enabled in [false, true] {
        let context = lifecycle.begin(
            request(ToolSurface::Mcp, None),
            RuntimeContext::default(),
            ProductEntitlements {
                autopilot: enabled,
                personalized_learning: enabled,
            },
        );
        assert_eq!(
            driver.ask_autopilot(&context).await.unwrap(),
            StageDisposition::Skipped(if enabled {
                "no canonical autopilot decision"
            } else {
                "not entitled"
            })
        );
    }
}

#[test]
fn compatibility_completion_does_not_invent_an_autopilot_decision() {
    for surface in [
        ToolSurface::Mcp,
        ToolSurface::Cli,
        ToolSurface::Daemon,
        ToolSurface::Hook,
        ToolSurface::Proxy,
    ] {
        let lifecycle = ExecutionLifecycle::default();
        let context = lifecycle.begin(
            request(surface, None),
            RuntimeContext::default(),
            ProductEntitlements {
                autopilot: true,
                personalized_learning: true,
            },
        );
        lifecycle.complete(
            &context,
            CompletionObservation::tool_result(1, 1, "test", true),
        );
        assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
        assert_eq!(
            context.stage_executions()[5].disposition,
            StageDisposition::Skipped("no canonical autopilot decision")
        );
        assert_eq!(
            context.outcome().unwrap().accepted_outcome.accepted,
            AcceptanceState::Unknown
        );
    }
}

#[test]
fn transport_status_without_evidence_is_unknown() {
    for surface in [ToolSurface::Mcp, ToolSurface::Cli, ToolSurface::Proxy] {
        for success in [true, false] {
            let lifecycle = ExecutionLifecycle::default();
            let context = lifecycle.begin(
                request(surface, None),
                RuntimeContext::default(),
                ProductEntitlements::default(),
            );
            let mut observation = CompletionObservation::tool_result(1, 1, "test", success);
            observation.outcome_signals.clear();
            let outcome = lifecycle.complete(&context, observation.clone());

            assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
            assert_eq!(
                outcome.accepted_outcome.task_id.as_str(),
                context.task_id.as_str()
            );
            assert!(outcome.accepted_outcome.evidence_refs.is_empty());
            assert!(outcome.assessment.is_none());
            assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
            assert!(lifecycle.complete_once(&context, observation).is_none());
        }
    }
}

async fn finalizer_fault_case(
    fault: Option<LifecycleStage>,
    panic_flush: bool,
    signals: Vec<OutcomeSignal>,
    expected: AcceptanceState,
) {
    use futures::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let lifecycle = ExecutionLifecycle::default();
    let request = request(ToolSurface::Proxy, Some("finalizer-fault"));
    let context = lifecycle.begin(
        request.clone(),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let computations = Arc::new(AtomicUsize::new(0));
    let telemetry = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(None));
    let weak_spine = Arc::downgrade(&lifecycle.spine);
    let task_id = context.task_id.clone();
    let calls = computations.clone();
    let telemetry_calls = telemetry.clone();
    let before = captured.clone();
    *context.shared.finalization_hook.lock().unwrap() = Some(Arc::new(move |stage, outcome| {
        if stage == LifecycleStage::RecordEvidence {
            calls.fetch_add(1, Ordering::SeqCst);
        }
        if stage == LifecycleStage::ScheduleTelemetry {
            telemetry_calls.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(outcome) = outcome {
            *before.lock().unwrap() = Some(outcome.clone());
        }
        if fault == Some(stage) {
            // A completed outcome must survive independently of its evictable
            // lookup projection; a missing projection is not failed quality.
            weak_spine
                .upgrade()
                .unwrap()
                .outcomes
                .lock()
                .unwrap()
                .remove(&task_id);
            panic!("injected finalizer failure at {stage:?}");
        }
    }));
    let flushes = Arc::new(AtomicUsize::new(0));
    let calls = flushes.clone();
    *context.shared.flush_hook.lock().unwrap() = Some(Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        assert!(!panic_flush, "injected fallback flush failure after write");
    }));
    let dispatches = Arc::new(AtomicUsize::new(0));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        std::panic::AssertUnwindSafe(lifecycle.run(
            request.clone(),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            SlowDriver {
                dispatches: dispatches.clone(),
                signals: signals.clone(),
            },
        ))
        .catch_unwind(),
    )
    .await
    .expect("finalizer and recovery must terminate");
    assert!(
        result.is_ok(),
        "recovery must not unwind the completed primitive"
    );
    assert_eq!(result.unwrap().unwrap(), 7);
    let outcome = context
        .outcome()
        .expect("recovery must publish terminal state");
    assert_eq!(outcome.accepted_outcome.accepted, expected);
    assert_eq!(
        outcome.assessment.is_none(),
        expected == AcceptanceState::Unknown
    );
    if let Some(before) = captured.lock().unwrap().as_ref() {
        assert_eq!(outcome.accepted_outcome, before.accepted_outcome);
        assert_eq!(
            serde_json::to_value(&outcome.assessment).unwrap(),
            serde_json::to_value(&before.assessment).unwrap()
        );
    }
    assert_eq!(computations.load(Ordering::SeqCst), 1);
    assert_eq!(telemetry.load(Ordering::SeqCst), 1);
    assert_eq!(flushes.load(Ordering::SeqCst), 1);
    assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            lifecycle.run(
                request,
                RuntimeContext::default(),
                ProductEntitlements::default(),
                SlowDriver {
                    dispatches: dispatches.clone(),
                    signals,
                }
            )
        )
        .await
        .expect("terminal replay must not wait or redispatch")
        .unwrap(),
        7
    );
    assert_eq!(dispatches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn finalizer_control_preserves_unknown_and_once_effects() {
    finalizer_fault_case(None, false, vec![], AcceptanceState::Unknown).await;
}

#[tokio::test]
async fn finalizer_panic_before_outcome_does_not_fabricate_quality() {
    finalizer_fault_case(
        Some(LifecycleStage::RecordEvidence),
        false,
        vec![],
        AcceptanceState::Unknown,
    )
    .await;
}

#[tokio::test]
async fn finalizer_panic_after_outcome_preserves_evicted_result() {
    finalizer_fault_case(
        Some(LifecycleStage::UpdateBreakEven),
        false,
        vec![],
        AcceptanceState::Unknown,
    )
    .await;
}

#[tokio::test]
async fn finalizer_panic_with_flush_panic_still_terminalizes_once() {
    finalizer_fault_case(
        Some(LifecycleStage::RecordEvidence),
        true,
        vec![],
        AcceptanceState::Unknown,
    )
    .await;
}

#[tokio::test]
async fn finalizer_panic_preserves_supplied_acceptance_evidence() {
    finalizer_fault_case(
        Some(LifecycleStage::UpdateBreakEven),
        false,
        vec![OutcomeSignal::UserAccepted],
        AcceptanceState::Accepted,
    )
    .await;
}

#[tokio::test]
async fn finalizer_panic_preserves_supplied_rejection_evidence() {
    finalizer_fault_case(
        Some(LifecycleStage::UpdateBreakEven),
        false,
        vec![OutcomeSignal::UserRejected],
        AcceptanceState::Rejected,
    )
    .await;
}

#[tokio::test]
async fn finalizer_telemetry_panic_is_not_retried() {
    finalizer_fault_case(
        Some(LifecycleStage::ScheduleTelemetry),
        false,
        vec![],
        AcceptanceState::Unknown,
    )
    .await;
}

#[test]
fn partial_flush_panic_is_not_retried_by_recovery_or_guard_drop() {
    for owned in [false, true] {
        let lifecycle = ExecutionLifecycle::default();
        let context = lifecycle.begin(
            request(ToolSurface::Cli, None),
            RuntimeContext::default(),
            ProductEntitlements::default(),
        );
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = attempts.clone();
        *context.shared.flush_hook.lock().unwrap() = Some(Arc::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("flush failed after a partial side effect");
        }));
        lifecycle.prepare(&context);
        let guard = if owned {
            LifecycleGuard::for_run(&lifecycle, context.clone(), None)
        } else {
            LifecycleGuard::new(&lifecycle, context.clone())
        };
        let outcome = guard.complete(CompletionObservation::tool_result(0, 0, "local", true));
        drop(guard);
        assert!(
            lifecycle
                .complete_once(
                    &context,
                    CompletionObservation::tool_result(0, 0, "local", true),
                )
                .is_none()
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The reusable injection catches retries, including panic recovery.
        assert!(
            context
                .shared
                .flush_started
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
        assert_eq!(
            context.stage_executions().last().unwrap().disposition,
            StageDisposition::Skipped("state flush panicked")
        );
        assert_eq!(
            context.outcome().unwrap().accepted_outcome,
            outcome.accepted_outcome
        );
    }
}

#[test]
fn failed_transport_without_quality_signals_remains_unknown() {
    let lifecycle = ExecutionLifecycle::default();
    let context = lifecycle.begin(
        request(ToolSurface::Proxy, None),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let observation = CompletionObservation::tool_result(0, 0, "unknown", false);
    assert!(observation.outcome_signals.is_empty());
    let outcome = lifecycle.complete(&context, observation);
    assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
    assert!(outcome.assessment.is_none());
    assert!(outcome.accepted_outcome.evidence_refs.is_empty());
    assert_outcome_lineage(&context, &outcome);
}

#[test]
fn empty_signals_remain_unknown_for_every_canonical_contract() {
    for class in [
        TaskClass::BugFix,
        TaskClass::Refactor,
        TaskClass::TestAddition,
        TaskClass::Documentation,
        TaskClass::Investigation,
    ] {
        let contract = OutcomeContractV1::for_task_class(class);
        let evaluation = OutcomeEvaluator::new().evaluate_with_context(
            &contract,
            &[],
            crate::core::outcome::evaluator::EvaluationContext::new(
                "empty-evidence-task",
                &contract.contract_version,
                crate::core::outcome::evaluator::EVALUATOR_VERSION,
            ),
        );
        assert_eq!(
            evaluation.outcome.accepted,
            AcceptanceState::Unknown,
            "{class}"
        );
        assert!(evaluation.outcome.evidence_refs.is_empty(), "{class}");
    }
}

fn assert_outcome_lineage(context: &TaskContext, outcome: &LifecycleOutcome) {
    let contract = OutcomeContractV1::for_task_class(task_class(&context.profile_intent));
    let expected = OutcomeEvaluator::new().evaluate_with_context(
        &contract,
        &[],
        crate::core::outcome::evaluator::EvaluationContext::new(
            &context.task_id,
            &contract.contract_version,
            crate::core::outcome::evaluator::EVALUATOR_VERSION,
        ),
    );
    assert_eq!(outcome.accepted_outcome.task_id.as_str(), context.task_id);
    assert_eq!(
        outcome.accepted_outcome.outcome_id,
        expected.outcome.outcome_id
    );
}

#[tokio::test]
async fn failed_run_retains_task_and_outcome_lineage_across_all_early_returns() {
    let lifecycle = ExecutionLifecycle::default();
    let mut identities = std::collections::HashSet::new();
    for stage in &LIFECYCLE_STAGE_ORDER[..10] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::new(Mutex::new(None));
        let key = format!("lineage-{stage:?}");
        let first = lifecycle
            .run(
                request(ToolSurface::Mcp, Some(&key)),
                RuntimeContext::default(),
                ProductEntitlements::default(),
                TestDriver {
                    calls: calls.clone(),
                    context: captured.clone(),
                    fail_at: Some(*stage),
                },
            )
            .await;
        assert!(matches!(first, Err(LifecycleRunError::Dispatch(_))));
        let context = captured.lock().unwrap().clone().unwrap();
        let outcome = context.outcome().unwrap();
        assert_outcome_lineage(&context, &outcome);
        assert!(identities.insert(outcome.accepted_outcome.outcome_id.as_str().to_owned()));
        assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
        assert!(outcome.assessment.is_none());
        assert!(outcome.accepted_outcome.evidence_refs.is_empty());
        assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
        let original_calls = calls.lock().unwrap().clone();
        let replay = lifecycle
            .run(
                request(ToolSurface::Mcp, Some(&key)),
                RuntimeContext::default(),
                ProductEntitlements::default(),
                TestDriver {
                    calls: calls.clone(),
                    context: captured.clone(),
                    fail_at: None,
                },
            )
            .await;
        match (first, replay) {
            (
                Err(LifecycleRunError::Dispatch(expected)),
                Err(LifecycleRunError::Dispatch(actual)),
            ) => {
                assert_eq!(actual, expected);
            }
            results => panic!("failed run was not replayed: {results:?}"),
        }
        assert_eq!(*calls.lock().unwrap(), original_calls);
        assert_eq!(
            context.outcome().unwrap().accepted_outcome,
            outcome.accepted_outcome
        );
    }
}

#[test]
fn failed_outcome_with_signals_retains_contract_lineage() {
    for (signal, expected) in [
        (OutcomeSignal::UserAccepted, AcceptanceState::Accepted),
        (OutcomeSignal::UserRejected, AcceptanceState::Rejected),
    ] {
        let lifecycle = ExecutionLifecycle::default();
        let context = lifecycle.begin(
            request(ToolSurface::Proxy, None),
            RuntimeContext::default(),
            ProductEntitlements::default(),
        );
        let mut observation = CompletionObservation::tool_result(0, 0, "unknown", false);
        observation.outcome_signals = vec![signal];
        let outcome = lifecycle.complete(&context, observation);
        assert_outcome_lineage(&context, &outcome);
        assert_eq!(outcome.accepted_outcome.accepted, expected);
        assert!(!outcome.accepted_outcome.evidence_refs.is_empty());
    }
}

#[test]
fn duplicate_completion_has_one_terminal_record() {
    let lifecycle = ExecutionLifecycle::default();
    let context = lifecycle.begin(
        request(ToolSurface::Hook, Some("trace-1")),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let observation = CompletionObservation::tool_result(1, 1, "test", true);

    let first = lifecycle
        .complete_once(&context, observation.clone())
        .expect("first completion owns finalization");
    assert!(
        lifecycle
            .complete_once(&context, observation.clone())
            .is_none()
    );
    let duplicate = lifecycle.complete(&context, observation);
    assert_eq!(first.accepted_outcome, duplicate.accepted_outcome);
}

#[test]
fn retry_key_reuses_task_lineage_within_security_scope() {
    let lifecycle = ExecutionLifecycle::default();
    let first = lifecycle.begin(
        request(ToolSurface::Cli, Some("retry-1")),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let retry = lifecycle.begin(
        request(ToolSurface::Cli, Some("retry-1")),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );

    assert_eq!(first.task_id, retry.task_id);
    assert!(Arc::ptr_eq(&first.shared, &retry.shared));
}

#[tokio::test]
async fn retry_key_dispatches_once_and_replays_result() {
    let lifecycle = ExecutionLifecycle::default();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let first = lifecycle
        .run(
            request(ToolSurface::Mcp, Some("same-operation")),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            TestDriver {
                calls: calls.clone(),
                context: Arc::new(Mutex::new(None)),
                fail_at: None,
            },
        )
        .await
        .unwrap();
    let replay = lifecycle
        .run(
            request(ToolSurface::Mcp, Some("same-operation")),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            TestDriver {
                calls: calls.clone(),
                context: Arc::new(Mutex::new(None)),
                fail_at: None,
            },
        )
        .await
        .unwrap();

    assert_eq!((first, replay), (8, 8));
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        11
    );
}

#[tokio::test]
async fn same_raw_key_in_different_security_scope_does_not_replay() {
    let lifecycle = ExecutionLifecycle::default();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let first_request = request(ToolSurface::Mcp, Some("caller-key"));
    let mut second_request = first_request.clone();
    second_request.agent_id = "different-agent".to_owned();
    second_request.query = Some("different payload".to_owned());
    for scoped_request in [first_request, second_request] {
        assert_eq!(
            lifecycle
                .run(
                    scoped_request,
                    RuntimeContext::default(),
                    ProductEntitlements::default(),
                    TestDriver {
                        calls: calls.clone(),
                        context: Arc::new(Mutex::new(None)),
                        fail_at: None,
                    },
                )
                .await
                .unwrap(),
            8
        );
    }
    assert_eq!(
        calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|stage| **stage == LifecycleStage::DispatchPrimitive)
            .count(),
        2
    );
}

#[tokio::test]
async fn replay_scope_preserves_field_boundaries_and_optional_values() {
    let base = request(ToolSurface::Mcp, Some("same-key"));
    let mut caller_a = base.clone();
    caller_a.session_id = "session:part".into();
    caller_a.agent_id = "agent".into();
    let mut caller_b = base.clone();
    caller_b.session_id = "session".into();
    caller_b.agent_id = "part:agent".into();
    let mut absent_query = base.clone();
    absent_query.query = None;
    let mut empty_query = base.clone();
    empty_query.query = Some(String::new());
    let cases = [
        (
            (caller_a, RuntimeContext::default()),
            (caller_b, RuntimeContext::default()),
        ),
        (
            (absent_query, RuntimeContext::default()),
            (empty_query, RuntimeContext::default()),
        ),
        (
            (
                base.clone(),
                RuntimeContext {
                    client_name: Some("client:part".into()),
                    project_root: Some("workspace".into()),
                },
            ),
            (
                base.clone(),
                RuntimeContext {
                    client_name: Some("client".into()),
                    project_root: Some("part:workspace".into()),
                },
            ),
        ),
        (
            (base.clone(), RuntimeContext::default()),
            (
                base,
                RuntimeContext {
                    client_name: Some(String::new()),
                    project_root: None,
                },
            ),
        ),
    ];
    for (first, second) in cases {
        let lifecycle = ExecutionLifecycle::default();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut task_ids = std::collections::HashSet::new();
        for (request, runtime) in [first, second] {
            let captured = Arc::new(Mutex::new(None));
            let result = lifecycle
                .run(
                    request,
                    runtime,
                    ProductEntitlements::default(),
                    TestDriver {
                        calls: calls.clone(),
                        context: captured.clone(),
                        fail_at: None,
                    },
                )
                .await;
            assert!(matches!(result, Ok(8)));
            let context = captured.lock().unwrap().clone();
            assert!(context.is_some(), "distinct scope replayed another task");
            assert!(task_ids.insert(context.unwrap().task_id));
        }
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|stage| **stage == LifecycleStage::DispatchPrimitive)
                .count(),
            2
        );
    }
}

#[tokio::test]
async fn replay_type_mismatch_fails_closed_without_poisoning_original_result() {
    let lifecycle = ExecutionLifecycle::default();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::new(Mutex::new(None));
    let result = lifecycle
        .run(
            request(ToolSurface::Mcp, Some("typed-replay")),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            TestDriver {
                calls: calls.clone(),
                context: captured.clone(),
                fail_at: None,
            },
        )
        .await;
    assert!(matches!(result, Ok(8)));
    let context = captured.lock().unwrap().clone().unwrap();
    let outcome = context.outcome().unwrap();
    let original_calls = calls.lock().unwrap().clone();
    let mismatch = context.cached_or_claim_dispatch::<String, String>().await;
    assert!(matches!(
        mismatch,
        Some(Err(LifecycleRunError::ReplayTypeMismatch))
    ));
    let validations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mismatch = lifecycle
        .run(
            request(ToolSurface::Mcp, Some("typed-replay")),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            MismatchedReplayDriver {
                inner: TestDriver {
                    calls: calls.clone(),
                    context: captured,
                    fail_at: None,
                },
                validations: validations.clone(),
            },
        )
        .await;
    assert!(matches!(
        mismatch,
        Err(LifecycleRunError::ReplayTypeMismatch)
    ));
    assert_eq!(validations.load(std::sync::atomic::Ordering::SeqCst), 0);
    let replay = context.cached_or_claim_dispatch::<u8, String>().await;
    assert!(matches!(replay, Some(Ok(8))));
    assert_eq!(*calls.lock().unwrap(), original_calls);
    assert_eq!(
        context.outcome().unwrap().accepted_outcome,
        outcome.accepted_outcome
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 10)]
async fn concurrent_same_key_dispatches_once() {
    let lifecycle = Arc::new(ExecutionLifecycle::default());
    let dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let lifecycle = lifecycle.clone();
        let dispatches = dispatches.clone();
        tasks.push(tokio::spawn(async move {
            lifecycle
                .run(
                    request(ToolSurface::Mcp, Some("concurrent-operation")),
                    RuntimeContext::default(),
                    ProductEntitlements::default(),
                    SlowDriver {
                        dispatches,
                        signals: Vec::new(),
                    },
                )
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), 7);
    }
    assert_eq!(dispatches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

async fn checkpoint_cancellation(panic_flush: bool) {
    let lifecycle = Arc::new(ExecutionLifecycle::default());
    let reached = Arc::new(tokio::sync::Notify::new());
    let cancellations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wait_until_checkpoint = reached.notified();
    let worker = {
        let lifecycle = lifecycle.clone();
        let reached = reached.clone();
        let cancellations = cancellations.clone();
        tokio::spawn(async move {
            lifecycle
                .run(
                    request(ToolSurface::Mcp, Some("cancelled-checkpoint")),
                    RuntimeContext::default(),
                    ProductEntitlements::default(),
                    BlockingCheckpointDriver {
                        reached,
                        cancellations,
                    },
                )
                .await
        })
    };
    wait_until_checkpoint.await;
    let context = lifecycle.begin(
        request(ToolSurface::Mcp, Some("cancelled-checkpoint")),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let before = lifecycle.outcome_for(&context.task_id).unwrap();
    let flushes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    if panic_flush {
        let flushes = flushes.clone();
        *context.shared.flush_hook.lock().unwrap() = Some(Arc::new(move || {
            flushes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("checkpoint cancellation flush failed after write");
        }));
    }
    // The guard owns its completed outcome; lookup eviction cannot change
    // cancellation semantics or cause economics to run a second time.
    lifecycle
        .spine
        .outcomes
        .lock()
        .unwrap()
        .remove(&context.task_id);
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    assert!(context.outcome().is_some());
    let after = context.outcome().unwrap();
    assert_eq!(after.accepted_outcome, before.accepted_outcome);
    assert_eq!(
        lifecycle
            .outcome_for(&context.task_id)
            .unwrap()
            .accepted_outcome,
        before.accepted_outcome
    );
    assert_eq!(after.assessment.is_none(), before.assessment.is_none());
    assert_eq!(cancellations.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        flushes.load(std::sync::atomic::Ordering::SeqCst),
        usize::from(panic_flush)
    );
    assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
    let replay = lifecycle
        .run(
            request(ToolSurface::Mcp, Some("cancelled-checkpoint")),
            RuntimeContext::default(),
            ProductEntitlements::default(),
            SlowDriver {
                dispatches: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                signals: Vec::new(),
            },
        )
        .await;
    assert!(matches!(replay, Err(LifecycleRunError::Aborted(_))));
}

#[tokio::test]
async fn cancellation_during_checkpoint_terminalizes_without_deadlock() {
    checkpoint_cancellation(false).await;
}

#[tokio::test]
async fn checkpoint_cancellation_preserves_outcome_after_flush_panic() {
    checkpoint_cancellation(true).await;
}

#[test]
fn parallel_completion_returns_one_terminal_outcome() {
    let lifecycle = Arc::new(ExecutionLifecycle::default());
    let context = lifecycle.begin(
        request(ToolSurface::Cli, None),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let mut workers = Vec::new();
    for _ in 0..8 {
        let lifecycle = lifecycle.clone();
        let context = context.clone();
        workers.push(std::thread::spawn(move || {
            lifecycle.complete(
                &context,
                CompletionObservation::tool_result(1, 1, "test", true),
            )
        }));
    }
    let outcomes: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().expect("completion worker"))
        .collect();

    assert!(outcomes.windows(2).all(|pair| {
        pair[0].accepted_outcome.outcome_id == pair[1].accepted_outcome.outcome_id
    }));
    assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
}

#[test]
fn cancellation_hook_panic_still_terminalizes_once_without_quality_evidence() {
    let lifecycle = ExecutionLifecycle::default();
    let context = lifecycle.begin(
        request(ToolSurface::Proxy, None),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    for _ in 0..2 {
        let calls = calls.clone();
        drop(LifecycleGuard::for_run(
            &lifecycle,
            context.clone(),
            Some(Box::new(move || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                panic!("cancellation callback failed");
            })),
        ));
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let outcome = context.outcome().unwrap();
    assert_outcome_lineage(&context, &outcome);
    assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
    assert!(outcome.assessment.is_none());
    assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
}

#[test]
fn cancellation_flush_panic_preserves_unknown_and_does_not_retry_write() {
    let lifecycle = ExecutionLifecycle::default();
    let context = lifecycle.begin(
        request(ToolSurface::Proxy, None),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    let flushes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = flushes.clone();
    *context.shared.flush_hook.lock().unwrap() = Some(Arc::new(move || {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("cancelled dispatch flush failed after write");
    }));
    for _ in 0..2 {
        drop(LifecycleGuard::for_run(&lifecycle, context.clone(), None));
    }
    assert_eq!(flushes.load(std::sync::atomic::Ordering::SeqCst), 1);
    let outcome = context.outcome().unwrap();
    assert_outcome_lineage(&context, &outcome);
    assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
    assert!(outcome.assessment.is_none());
    assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
}

#[test]
fn guard_finalizes_early_return_once() {
    let lifecycle = ExecutionLifecycle::default();
    let context = lifecycle.begin(
        request(ToolSurface::Mcp, None),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    {
        let _guard = LifecycleGuard::new(&lifecycle, context.clone());
    }

    let outcome = context.outcome().expect("drop records terminal outcome");
    assert_outcome_lineage(&context, &outcome);
    assert_eq!(outcome.accepted_outcome.accepted, AcceptanceState::Unknown);
    assert!(outcome.assessment.is_none());
    let duplicate = lifecycle.complete(
        &context,
        CompletionObservation::tool_result(1, 1, "test", true),
    );
    assert_eq!(
        duplicate.accepted_outcome.accepted,
        AcceptanceState::Unknown
    );
    assert_eq!(
        context.stage_executions()[5].disposition,
        StageDisposition::Skipped("not entitled")
    );
}

#[test]
fn replaying_an_earlier_stage_does_not_skip_forward() {
    let lifecycle = ExecutionLifecycle::default();
    let context = lifecycle.begin(
        request(ToolSurface::Mcp, None),
        RuntimeContext::default(),
        ProductEntitlements::default(),
    );
    context.advance_through(LifecycleStage::ApplySecurityBoundaries);
    context.advance_through(LifecycleStage::ResolveEntitlements);

    assert_eq!(
        context.completed_stages(),
        LIFECYCLE_STAGE_ORDER[..=3].to_vec()
    );
}

#[test]
fn every_surface_uses_identical_stage_order() {
    for surface in [
        ToolSurface::Mcp,
        ToolSurface::Cli,
        ToolSurface::Daemon,
        ToolSurface::Hook,
        ToolSurface::Proxy,
    ] {
        let lifecycle = ExecutionLifecycle::default();
        let context = lifecycle.begin(
            request(surface, None),
            RuntimeContext::default(),
            ProductEntitlements::default(),
        );
        lifecycle.complete(
            &context,
            CompletionObservation::tool_result(0, 0, "test", true),
        );
        assert_eq!(context.completed_stages(), LIFECYCLE_STAGE_ORDER);
        assert_eq!(
            context.outcome().unwrap().accepted_outcome.accepted,
            AcceptanceState::Unknown
        );
    }
}
