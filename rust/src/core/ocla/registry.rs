//! OclaRegistry — singleton that wires all 15 builtin trait implementations.
//!
//! Provides `OclaRegistry::global()` for production code to access any OCLA
//! capability through its trait interface. The Strangler Fig adoption pattern
//! means existing call sites can be migrated one-by-one to use the registry
//! instead of calling internal modules directly.

use std::sync::{Arc, OnceLock};

#[cfg(test)]
use std::cell::Cell;

use super::builtin::{
    agent_gateway::BuiltinAgentGateway, compression_provider::BuiltinCompressionProvider,
    config_tuner::BuiltinConfigTuner, connector_scheduler::BuiltinConnectorScheduler,
    delivery_registry::BuiltinDeliveryRegistry, efficiency_analyzer::BuiltinEfficiencyAnalyzer,
    experiment_runner::BuiltinExperimentRunner, intent_classifier::BuiltinIntentClassifier,
    metrics_exporter::BuiltinMetricsExporter, model_router::BuiltinModelRouter,
    observation_hook::BuiltinObservationHook, outcome_tracker::BuiltinOutcomeTracker,
    response_optimizer::BuiltinResponseOptimizer, savings_ledger::BuiltinSavingsLedger,
    usage_sink::BuiltinUsageSink,
};
use super::traits::{
    AgentGateway, CompressionProvider, ConfigTuner, ConnectorScheduler, DeliveryRegistry,
    EfficiencyAnalyzer, ExperimentRunner, IntentClassifier, MetricsExporter, ModelRouter,
    ObservationHook, OutcomeTracker, ResponseOptimizer, SavingsLedger, UsageSink,
};

static GLOBAL_REGISTRY: OnceLock<OclaRegistry> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST_REGISTRY: Cell<*mut OclaRegistry> = const { Cell::new(std::ptr::null_mut()) };
}

pub struct OclaRegistry {
    pub observation_hook: Arc<dyn ObservationHook>,
    pub usage_sink: Arc<dyn UsageSink>,
    pub metrics_exporter: Arc<dyn MetricsExporter>,
    pub savings_ledger: Arc<dyn SavingsLedger>,
    pub intent_classifier: Arc<dyn IntentClassifier>,
    pub outcome_tracker: Arc<dyn OutcomeTracker>,
    pub compression_provider: Arc<dyn CompressionProvider>,
    pub response_optimizer: Arc<dyn ResponseOptimizer>,
    pub model_router: Arc<dyn ModelRouter>,
    pub efficiency_analyzer: Arc<dyn EfficiencyAnalyzer>,
    pub config_tuner: Arc<dyn ConfigTuner>,
    pub experiment_runner: Arc<dyn ExperimentRunner>,
    pub connector_scheduler: Arc<dyn ConnectorScheduler>,
    pub agent_gateway: Arc<dyn AgentGateway>,
    pub delivery_registry: Arc<dyn DeliveryRegistry>,
}

impl OclaRegistry {
    pub fn global() -> &'static Self {
        #[cfg(test)]
        if let Some(registry) = TEST_REGISTRY.with(|slot| {
            let ptr = slot.get();
            (!ptr.is_null()).then(|| {
                // Test registries are leaked until process exit and scoped to the
                // calling thread, so this pointer remains valid for the call.
                // SAFETY: The pointer references a leaked, thread-local registry.
                unsafe { &*ptr }
            })
        }) {
            return registry;
        }
        GLOBAL_REGISTRY.get_or_init(Self::with_builtins)
    }

    pub fn with_builtins() -> Self {
        Self {
            observation_hook: Arc::new(BuiltinObservationHook::new()),
            usage_sink: Arc::new(BuiltinUsageSink::new()),
            metrics_exporter: Arc::new(BuiltinMetricsExporter::new()),
            savings_ledger: Arc::new(BuiltinSavingsLedger::new()),
            intent_classifier: Arc::new(BuiltinIntentClassifier::new()),
            outcome_tracker: Arc::new(BuiltinOutcomeTracker::new()),
            compression_provider: Arc::new(BuiltinCompressionProvider::new()),
            response_optimizer: Arc::new(BuiltinResponseOptimizer::new()),
            model_router: Arc::new(BuiltinModelRouter::new()),
            efficiency_analyzer: Arc::new(BuiltinEfficiencyAnalyzer::new()),
            config_tuner: Arc::new(BuiltinConfigTuner::new()),
            experiment_runner: Arc::new(BuiltinExperimentRunner::new()),
            connector_scheduler: Arc::new(BuiltinConnectorScheduler::new()),
            agent_gateway: Arc::new(BuiltinAgentGateway::new()),
            delivery_registry: Arc::new(BuiltinDeliveryRegistry::new()),
        }
    }
}

#[cfg(test)]
pub(crate) struct TestRegistryGuard {
    previous: *mut OclaRegistry,
}

#[cfg(test)]
pub(crate) fn with_test_registry(registry: OclaRegistry) -> TestRegistryGuard {
    let registry = Box::leak(Box::new(registry));
    let previous = TEST_REGISTRY.with(|slot| slot.replace(registry));
    TestRegistryGuard { previous }
}

#[cfg(test)]
impl Drop for TestRegistryGuard {
    fn drop(&mut self) {
        TEST_REGISTRY.with(|slot| slot.set(self.previous));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::traits::{
        EfficiencyAnalyzer, ObservationHook, OclaService, OutcomeTracker, SavingsLedger, UsageSink,
    };
    use crate::core::ocla::types::{
        AgentEnvelope, CompressionRequest, ConfigTuningRequest, ConnectorJob, EfficiencyAnalysis,
        EfficiencySample, ExperimentRequest, IntentRequest, MetricPoint, ModelRouteRequest,
        Observation, OclaCapabilityKind, OclaCapabilityStatus, OclaRequestContext, OclaResult,
        Outcome, ResponseOptimizationRequest, SavingsEvidence, UsageRecord,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SpyEfficiency(Arc<AtomicUsize>);
    struct SpySavings(Arc<AtomicUsize>);
    struct SpyObservation(Arc<AtomicUsize>);
    struct SpyOutcome(Arc<AtomicUsize>);
    struct SpyUsage(Arc<AtomicUsize>);

    macro_rules! spy_capability {
        ($name:ty, $kind:expr) => {
            impl OclaService for $name {
                fn capability(&self) -> super::super::types::OclaCapability {
                    super::super::types::OclaCapability::available($kind)
                }
            }
        };
    }

    spy_capability!(SpyEfficiency, OclaCapabilityKind::EfficiencyAnalyzer);
    spy_capability!(SpySavings, OclaCapabilityKind::SavingsLedger);
    spy_capability!(SpyObservation, OclaCapabilityKind::ObservationHook);
    spy_capability!(SpyOutcome, OclaCapabilityKind::OutcomeTracker);
    spy_capability!(SpyUsage, OclaCapabilityKind::UsageSink);

    impl EfficiencyAnalyzer for SpyEfficiency {
        fn analyze_efficiency(&self, sample: EfficiencySample) -> OclaResult<EfficiencyAnalysis> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(EfficiencyAnalysis {
                etpao_milli: sample.accepted.map(|_| sample.delivered_tokens),
                duplicate_ratio_milli: 0,
                compression_rate_milli: 0,
                cache_hit_rate_milli: 0,
                recommendation_refs: Vec::new(),
            })
        }
    }

    impl SavingsLedger for SpySavings {
        fn record_savings(&self, evidence: SavingsEvidence) -> OclaResult<String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(evidence.evidence_ref)
        }
    }

    impl ObservationHook for SpyObservation {
        fn observe(&self, _observation: Observation) -> OclaResult<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    impl OutcomeTracker for SpyOutcome {
        fn record_outcome(&self, _outcome: Outcome) -> OclaResult<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    impl UsageSink for SpyUsage {
        fn record_usage(&self, _usage: UsageRecord) -> OclaResult<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn context() -> OclaRequestContext {
        OclaRequestContext {
            request_id: "request-1".into(),
            session_id: "session-1".into(),
            agent_id: "agent-1".into(),
            content_ref: "file:test.rs".into(),
            tenant_id: None,
            trace_id: "tr-unit".into(),
        }
    }

    #[test]
    fn registry_exposes_all_fifteen_capabilities() {
        let reg = OclaRegistry::with_builtins();
        assert_eq!(
            reg.observation_hook.capability().kind,
            OclaCapabilityKind::ObservationHook
        );
        assert_eq!(
            reg.agent_gateway.capability().kind,
            OclaCapabilityKind::AgentGateway
        );
        assert_eq!(
            reg.model_router.capability().kind,
            OclaCapabilityKind::ModelRouter
        );
        assert_eq!(
            reg.delivery_registry.capability().kind,
            OclaCapabilityKind::DeliveryRegistry
        );
    }

    #[test]
    fn all_capabilities_available() {
        let reg = OclaRegistry::with_builtins();
        assert_eq!(
            reg.observation_hook.capability().status,
            OclaCapabilityStatus::Available
        );
        assert_eq!(
            reg.savings_ledger.capability().status,
            OclaCapabilityStatus::Available
        );
    }

    #[test]
    fn global_registry_exposes_all_capabilities_as_available() {
        let reg = OclaRegistry::global();
        let capabilities = [
            (
                reg.observation_hook.capability(),
                OclaCapabilityKind::ObservationHook,
            ),
            (reg.usage_sink.capability(), OclaCapabilityKind::UsageSink),
            (
                reg.metrics_exporter.capability(),
                OclaCapabilityKind::MetricsExporter,
            ),
            (
                reg.savings_ledger.capability(),
                OclaCapabilityKind::SavingsLedger,
            ),
            (
                reg.intent_classifier.capability(),
                OclaCapabilityKind::IntentClassifier,
            ),
            (
                reg.outcome_tracker.capability(),
                OclaCapabilityKind::OutcomeTracker,
            ),
            (
                reg.compression_provider.capability(),
                OclaCapabilityKind::CompressionProvider,
            ),
            (
                reg.response_optimizer.capability(),
                OclaCapabilityKind::ResponseOptimizer,
            ),
            (
                reg.model_router.capability(),
                OclaCapabilityKind::ModelRouter,
            ),
            (
                reg.efficiency_analyzer.capability(),
                OclaCapabilityKind::EfficiencyAnalyzer,
            ),
            (
                reg.config_tuner.capability(),
                OclaCapabilityKind::ConfigTuner,
            ),
            (
                reg.experiment_runner.capability(),
                OclaCapabilityKind::ExperimentRunner,
            ),
            (
                reg.connector_scheduler.capability(),
                OclaCapabilityKind::ConnectorScheduler,
            ),
            (
                reg.agent_gateway.capability(),
                OclaCapabilityKind::AgentGateway,
            ),
            (
                reg.delivery_registry.capability(),
                OclaCapabilityKind::DeliveryRegistry,
            ),
        ];

        assert_eq!(capabilities.len(), OclaCapabilityKind::ALL.len());
        for (capability, expected_kind) in capabilities {
            assert_eq!(capability.kind, expected_kind);
            assert_eq!(capability.status, OclaCapabilityStatus::Available);
        }
    }

    #[test]
    fn global_registry_is_a_singleton() {
        assert!(std::ptr::eq(OclaRegistry::global(), OclaRegistry::global()));
    }

    #[test]
    fn every_builtin_main_method_can_be_called_without_panicking() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let reg = OclaRegistry::with_builtins();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = reg.observation_hook.observe(Observation {
                context: context(),
                name: "registry-smoke".into(),
                attributes: BTreeMap::new(),
            });
            let _ = reg.usage_sink.record_usage(UsageRecord {
                context: context(),
                model: "test-model".into(),
                input_tokens: 10,
                output_tokens: 5,
                provider_billed_tokens: 15,
            });
            let _ = reg.metrics_exporter.export_metrics(vec![MetricPoint {
                context: context(),
                name: "registry-smoke".into(),
                value_milli: 1,
                dimensions: BTreeMap::new(),
            }]);
            let _ = reg.savings_ledger.record_savings(SavingsEvidence {
                context: context(),
                original_tokens: 10,
                delivered_tokens: 5,
                quality_ref: None,
                evidence_ref: "evidence:registry-smoke".into(),
            });
            let _ = reg.intent_classifier.classify_intent(IntentRequest {
                context: context(),
                candidate_intents: vec!["review".into()],
            });
            let _ = reg.outcome_tracker.record_outcome(Outcome {
                context: context(),
                accepted: Some(true),
                quality_score_milli: Some(900),
                outcome_ref: Some("outcome:registry-smoke".into()),
            });
            let _ = reg.compression_provider.compress(CompressionRequest {
                context: context(),
                source_ref: "mem:registry-smoke".into(),
                source_tokens: 10,
                target_tokens: 5,
                quality_policy_ref: None,
            });
            let _ = reg
                .response_optimizer
                .optimize_response(ResponseOptimizationRequest {
                    context: context(),
                    response_ref: "response:registry-smoke".into(),
                    original_tokens: 10,
                    target_tokens: 5,
                });
            let _ = reg.model_router.route_model(ModelRouteRequest {
                context: context(),
                candidate_models: vec!["gpt-4o".into()],
                maximum_cost_micros: None,
                maximum_latency_ms: None,
            });
            let _ = reg
                .efficiency_analyzer
                .analyze_efficiency(EfficiencySample {
                    context: context(),
                    original_tokens: 10,
                    delivered_tokens: 5,
                    accepted: Some(true),
                    cache_hits: 0,
                    cache_reads: 0,
                });
            let _ = reg.config_tuner.propose_tuning(ConfigTuningRequest {
                context: context(),
                config_ref: "standard".into(),
                objective_ref: "minimize_tokens".into(),
            });
            let _ = reg.experiment_runner.run_experiment(ExperimentRequest {
                context: context(),
                experiment_ref: "missing-suite.ndjson".into(),
                cohort_ref: "cohort:registry-smoke".into(),
                holdout: None,
                stop_conditions: None,
            });
            let _ = reg.connector_scheduler.schedule_connector(ConnectorJob {
                context: context(),
                connector_id: "registry-smoke".into(),
                payload_ref: "payload:registry-smoke".into(),
                deadline_ms: Some(1_000),
            });
            let _ = reg.agent_gateway.relay_agent(AgentEnvelope {
                schema_version: 1,
                relay_id: "agent-relay:registry-smoke".into(),
                context: context(),
                from_agent_id: "agent-1".into(),
                to_agent_id: "agent-2".into(),
                capsule_ref: "capsule:registry-smoke".into(),
                budget_tokens: 100,
            });
            reg.delivery_registry
                .record_delivery(crate::core::ocla::types::DeliveryEntry {
                    blake3: [0u8; 12],
                    path: "test.rs".into(),
                    line_count: 42,
                    agent_id: "agent-1".into(),
                    conversation_id: "conv-1".into(),
                    mtime: 1000,
                    token_count: 12,
                });
            let _ = reg.delivery_registry.check_delivery(
                &[0u8; 12],
                1000,
                Some("x"),
                Some("agent-b"),
                None,
            );
            let _ = reg.delivery_registry.delivery_stats();
        }));

        assert!(result.is_ok(), "builtin method panicked");
    }

    #[test]
    fn cli_file_read_projects_to_efficiency_and_savings_spies() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let efficiency_calls = Arc::new(AtomicUsize::new(0));
        let savings_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = OclaRegistry::with_builtins();
        registry.efficiency_analyzer = Arc::new(SpyEfficiency(efficiency_calls.clone()));
        registry.savings_ledger = Arc::new(SpySavings(savings_calls.clone()));
        let _guard = with_test_registry(registry);

        crate::core::tool_lifecycle::record_file_read(
            "/tmp/ocla-cli-read.rs",
            "aggressive",
            1_000,
            375,
            false,
            std::time::Duration::from_millis(1),
            "fn main() {}",
        );

        assert_eq!(efficiency_calls.load(Ordering::Relaxed), 1);
        assert_eq!(savings_calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn mcp_tool_call_projects_to_observation_and_outcome_spies() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let observation_calls = Arc::new(AtomicUsize::new(0));
        let outcome_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = OclaRegistry::with_builtins();
        registry.observation_hook = Arc::new(SpyObservation(observation_calls.clone()));
        registry.outcome_tracker = Arc::new(SpyOutcome(outcome_calls.clone()));
        let _guard = with_test_registry(registry);
        let server = crate::tools::LeanCtxServer::new();

        server
            .record_call_with_timing("ctx_read", 100, 40, Some("full".into()), 3)
            .await;

        assert_eq!(observation_calls.load(Ordering::Relaxed), 1);
        assert_eq!(outcome_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn proxy_request_projects_measured_usage_to_usage_spy() {
        let _dir = crate::core::data_dir::isolated_data_dir();
        let usage_calls = Arc::new(AtomicUsize::new(0));
        let mut registry = OclaRegistry::with_builtins();
        registry.usage_sink = Arc::new(SpyUsage(usage_calls.clone()));
        let _guard = with_test_registry(registry);
        let usage = crate::proxy::usage::RealUsage {
            model: "test-model".into(),
            input_tokens: 11,
            output_tokens: 7,
            wire: Some(Box::new(crate::proxy::usage::WireContext {
                lineage: Some(context()),
                ..Default::default()
            })),
            ..Default::default()
        };

        crate::proxy::usage_meter::record(&usage);

        assert_eq!(usage_calls.load(Ordering::Relaxed), 1);
    }
}
