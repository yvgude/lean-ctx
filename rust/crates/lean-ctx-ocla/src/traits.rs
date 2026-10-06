// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use lean_ctx_protocol::CapabilityManifestV1;

use crate::types::{
    AgentEnvelope, AgentMessageRequest, CompressionRequest, CompressionResult, ConfigProposal,
    ConfigTuningRequest, ConnectorJob, DeliveryEntry, DeliveryRecord, DeliveryRecordResult,
    DeliveryStats, EfficiencyAnalysis, EfficiencySample, ExperimentRequest, ExperimentResult,
    IntentDecision, IntentRequest, MetricPoint, Observation, OclaCapability, OclaResult, Outcome,
    ResponseOptimizationRequest, ResponseOptimizationResult, SavingsEvidence, ScheduledJob,
    UsageRecord,
};

/// Common, versioned discovery surface for every OCLA capability.
pub trait OclaService: Send + Sync {
    fn capability(&self) -> OclaCapability;

    /// Returns an explicit versioned contract; missing declarations are unavailable.
    fn manifest(&self) -> OclaResult<CapabilityManifestV1> {
        Err(crate::types::OclaError::Unavailable(self.capability().kind))
    }
}

#[async_trait]
pub trait ObservationHook: OclaService {
    async fn observe(&self, observation: Observation) -> OclaResult<()>;
}

#[async_trait]
pub trait UsageSink: OclaService {
    async fn record_usage(&self, usage: UsageRecord) -> OclaResult<()>;
}

#[async_trait]
pub trait MetricsExporter: OclaService {
    async fn export_metrics(&self, metrics: Vec<MetricPoint>) -> OclaResult<()>;
}

pub trait SavingsLedger: OclaService {
    fn record_savings(&self, evidence: SavingsEvidence) -> OclaResult<String>;

    /// Update capability-local summaries and evidence for a caller-owned
    /// accounting observation, without appending another accounting event.
    fn project_savings(&self, evidence: SavingsEvidence) -> OclaResult<String>;
}

pub trait IntentClassifier: OclaService {
    fn classify_intent(&self, request: IntentRequest) -> OclaResult<IntentDecision>;
}

pub trait OutcomeTracker: OclaService {
    fn record_outcome(&self, outcome: Outcome) -> OclaResult<()>;
}

pub trait CompressionProvider: OclaService {
    fn compress(&self, request: CompressionRequest) -> OclaResult<CompressionResult>;
}

#[async_trait]
pub trait ResponseOptimizer: OclaService {
    async fn optimize_response(
        &self,
        request: ResponseOptimizationRequest,
    ) -> OclaResult<ResponseOptimizationResult>;
}

pub trait EfficiencyAnalyzer: OclaService {
    fn analyze_efficiency(&self, sample: EfficiencySample) -> OclaResult<EfficiencyAnalysis>;
}

pub trait ConfigTuner: OclaService {
    fn propose_tuning(&self, request: ConfigTuningRequest) -> OclaResult<ConfigProposal>;
}

pub trait ExperimentRunner: OclaService {
    fn run_experiment(&self, request: ExperimentRequest) -> OclaResult<ExperimentResult>;
}

pub trait ConnectorScheduler: OclaService {
    fn schedule_connector(&self, job: ConnectorJob) -> OclaResult<ScheduledJob>;
}

pub trait AgentGateway: OclaService {
    fn relay_agent(&self, envelope: AgentEnvelope) -> OclaResult<AgentEnvelope>;
    fn route_message(&self, request: &AgentMessageRequest) -> OclaResult<String>;
}

pub trait DeliveryRegistry: OclaService {
    /// Scope and requester identity must be established by the trusted caller.
    fn check_scoped_delivery(
        &self,
        blake3: &[u8; 12],
        path: &str,
        scope: &crate::delivery_scope::DeliveryScopeV1,
        requester_agent_id: &str,
        requester_conversation_id: Option<&str>,
    ) -> Option<DeliveryRecord>;

    fn check_delivery(
        &self,
        blake3: &[u8; 12],
        mtime: u64,
        path: &str,
        requester_agent_id: Option<&str>,
        requester_conversation_id: Option<&str>,
    ) -> Option<DeliveryRecord>;
    fn record_stub_served(&self, record: &DeliveryRecord, stub_tokens: u64);
    fn record_delivery(&self, entry: DeliveryEntry) -> DeliveryRecordResult;
    fn delivery_stats(&self) -> DeliveryStats;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::OclaCapabilityKind;

    struct UndeclaredService {
        kind: OclaCapabilityKind,
    }

    impl OclaService for UndeclaredService {
        fn capability(&self) -> OclaCapability {
            OclaCapability::available(self.kind)
        }
    }

    #[test]
    fn missing_manifest_is_unavailable_for_every_capability_kind() {
        for kind in OclaCapabilityKind::ALL {
            assert!(matches!(
                UndeclaredService { kind }.manifest(),
                Err(crate::types::OclaError::Unavailable(actual)) if actual == kind
            ));
        }
    }

    #[test]
    fn every_public_trait_is_object_safe() {
        fn assert_object_safe<T: ?Sized>() {}
        assert_object_safe::<dyn ObservationHook>();
        assert_object_safe::<dyn UsageSink>();
        assert_object_safe::<dyn MetricsExporter>();
        assert_object_safe::<dyn SavingsLedger>();
        assert_object_safe::<dyn IntentClassifier>();
        assert_object_safe::<dyn OutcomeTracker>();
        assert_object_safe::<dyn CompressionProvider>();
        assert_object_safe::<dyn ResponseOptimizer>();
        assert_object_safe::<dyn EfficiencyAnalyzer>();
        assert_object_safe::<dyn ConfigTuner>();
        assert_object_safe::<dyn ExperimentRunner>();
        assert_object_safe::<dyn ConnectorScheduler>();
        assert_object_safe::<dyn AgentGateway>();
        assert_object_safe::<dyn DeliveryRegistry>();
    }
}
