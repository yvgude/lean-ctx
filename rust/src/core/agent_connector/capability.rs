// SPDX-License-Identifier: Apache-2.0

//! Bind real CLI connectors to the existing capability registry and invocation.
//!
//! The host owns admission. Serialized invocation constraints may narrow it,
//! never grant remote access, enlarge a directory scope, or relax a deadline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use lean_ctx_protocol::CapabilityManifestV1;

use super::traits::{AgentConnector, AgentInfo, TaskRequest, TaskResult, TaskTermination};
use crate::core::ocla::adapters::AdapterRegistry;
use crate::core::ocla::capability_fabric::normalize_manifest;
use crate::core::ocla::invocation::{
    CapabilityAdapter, CapabilityFailureMode, CapabilityInput, CapabilityInvocation,
    CapabilityObservationV1, CapabilityResult, PolicyConstraints, evidence_ref,
};
use crate::core::ocla::{OclaError, OclaResult};

fn invalid(message: impl Into<String>) -> OclaError {
    OclaError::InvalidRequest(message.into())
}

/// Not deserializable: only trusted host code can create this admission.
pub(crate) struct AgentExecutionPolicy {
    root: PathBuf,
    constraints: PolicyConstraints,
    max_timeout_ms: u64,
}

impl AgentExecutionPolicy {
    pub(crate) fn new(
        root: &Path,
        constraints: PolicyConstraints,
        max_timeout_ms: u64,
    ) -> OclaResult<Self> {
        constraints.validate()?;
        if max_timeout_ms == 0 {
            return Err(invalid("host agent deadline must be positive"));
        }
        let root = root
            .canonicalize()
            .map_err(|_| invalid("invalid host agent root"))?;
        if !root.is_dir() {
            return Err(invalid("host agent root must be a directory"));
        }
        Ok(Self {
            root,
            constraints,
            max_timeout_ms,
        })
    }
}

/// One runtime object shared by discovery and actual LocalRunner dispatch.
pub(crate) struct AgentConnectorAdapter {
    connector: Arc<dyn AgentConnector>,
    info: AgentInfo,
    manifest: CapabilityManifestV1,
    host: AgentExecutionPolicy,
    model_selection: bool,
    turn_limit: bool,
}

impl AgentConnectorAdapter {
    pub(crate) fn new(
        connector: Arc<dyn AgentConnector>,
        host: AgentExecutionPolicy,
    ) -> OclaResult<Self> {
        let info = connector.info();
        if info.name != connector.name()
            || info.name.is_empty()
            || !info
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(invalid("invalid connector identity"));
        }
        let model_selection = connector.supports_model_selection();
        let turn_limit = connector.supports_turn_limit();
        // A local CLI is not proof of a local model. Remote is a conservative
        // risk boundary, not a claim about the configured provider or region.
        let manifest: CapabilityManifestV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "capability_id": format!("capability://leanctx/agent/{}", info.name),
            "provider": "unknown",
            "kind": "agent_connector",
            "version": "1.0.0",
            "surfaces": ["cli"],
            "support_matrix": {"cli": {"supported": true}},
            "local": true,
            "remote": true,
            "reversibility": "irreversible",
            "determinism": "non_deterministic",
            "data_movement": "remote",
            "supported_classifications": [],
            "measurement_support": {"latency": true, "tokens": true, "quality": false},
            "conformance_version": 1,
            "execution": "agent_cli",
            "connector": info.name,
            "connector_version": info.version,
            "backend": null,
            "token_measurement": "local_payload_tokenizer_not_provider_billing",
            "path_scope": "initial_working_directory_only_not_filesystem_sandbox",
            "controls": {
                "model_selection": model_selection,
                "max_turns": turn_limit,
                "process_deadline": true,
                "cancellation": false,
                "provider_token_limits": false
            }
        }))
        .map_err(|error| invalid(error.to_string()))?;
        let manifest = normalize_manifest(manifest).map_err(|error| invalid(error.to_string()))?;
        Ok(Self {
            connector,
            info,
            manifest,
            host,
            model_selection,
            turn_limit,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.info.name
    }

    pub(crate) fn invocation(&self, request: TaskRequest) -> CapabilityInvocation {
        CapabilityInvocation {
            task_id: request.id.clone(),
            capability_id: self.manifest.capability_id.as_str().to_owned(),
            capability_version: self.manifest.version.clone(),
            timeout_ms: request.timeout_ms,
            input: CapabilityInput::AgentTask(request),
            policy_constraints: self.host.constraints.clone(),
        }
    }

    fn check_policy(&self, policy: &PolicyConstraints, request: &TaskRequest) -> OclaResult<()> {
        policy.check_input(&CapabilityInput::AgentTask(request.clone()))?;
        if !policy.allow_remote {
            return Err(invalid(
                "agent backend is unknown; explicit host remote admission is required",
            ));
        }
        if policy.require_deterministic || policy.require_reversible {
            return Err(invalid(
                "agent execution cannot guarantee determinism or reversibility",
            ));
        }
        if !policy.allowed_data_classifications.is_empty() {
            return Err(invalid(
                "agent backend has no attested data classification support",
            ));
        }
        if policy.max_input_tokens.is_some() || policy.max_output_tokens.is_some() {
            return Err(invalid("agent CLI cannot enforce provider token limits"));
        }
        if policy
            .max_latency_ms
            .is_some_and(|limit| limit == 0 || request.timeout_ms > limit)
        {
            return Err(invalid("agent deadline exceeds policy latency limit"));
        }
        Ok(())
    }

    fn checked_request(&self, invocation: &CapabilityInvocation) -> OclaResult<TaskRequest> {
        invocation.validate()?;
        if invocation.capability_id != self.manifest.capability_id.as_str()
            || invocation.capability_version != self.manifest.version
        {
            return Err(invalid(
                "agent invocation does not match registered capability identity",
            ));
        }
        let CapabilityInput::AgentTask(request) = &invocation.input else {
            return Err(invalid(
                "agent connector accepts only the shared AgentTask input",
            ));
        };
        if invocation.task_id != request.id {
            return Err(invalid("agent task identity does not match invocation"));
        }
        super::traits::validate_request(request, self.turn_limit)
            .map_err(|error| invalid(error.to_string()))?;
        if request.model.is_some() && !self.model_selection {
            return Err(invalid("connector does not support model selection"));
        }
        if request.timeout_ms > self.host.max_timeout_ms
            || (invocation.timeout_ms != 0 && request.timeout_ms > invocation.timeout_ms)
        {
            return Err(invalid("agent deadline exceeds host or invocation bound"));
        }
        // Check both independently. A caller can never override the host policy.
        self.check_policy(&self.host.constraints, request)?;
        self.check_policy(&invocation.policy_constraints, request)?;
        let working_dir = request
            .working_dir
            .canonicalize()
            .map_err(|_| invalid("invalid agent working directory"))?;
        if !working_dir.is_dir() || !working_dir.starts_with(&self.host.root) {
            return Err(invalid(
                "agent working directory escapes admitted host root",
            ));
        }
        let mut request = request.clone();
        request.working_dir = working_dir;
        Ok(request)
    }

    /// The registry must contain this exact object, not a descriptor or lookalike.
    pub(crate) fn execute_registered(
        self: &Arc<Self>,
        registry: &AdapterRegistry,
        invocation: &CapabilityInvocation,
    ) -> OclaResult<(TaskResult, CapabilityResult)> {
        self.checked_request(invocation)?;
        let registered = registry
            .lookup(&invocation.capability_id, &invocation.capability_version)
            .ok_or_else(|| invalid("agent capability has no registered runtime"))?;
        let expected: Arc<dyn CapabilityAdapter> = self.clone();
        if !Arc::ptr_eq(&registered, &expected) {
            return Err(invalid("agent capability is bound to a different runtime"));
        }
        // The same object's live health and host admission below define the
        // surviving executable set. No descriptor/empty-set fallback can spawn.
        self.execute_checked(invocation)
    }

    fn execute_checked(
        &self,
        invocation: &CapabilityInvocation,
    ) -> OclaResult<(TaskResult, CapabilityResult)> {
        let start = Instant::now();
        let mut request = self.checked_request(invocation)?;
        let deadline_ms = request.timeout_ms;
        if !self.runtime_unchanged()
            || !self
                .connector
                .health_check_with_timeout(deadline_ms)
                .unwrap_or(false)
        {
            return Err(invalid("agent runtime is unavailable or changed"));
        }
        request.timeout_ms = deadline_ms.saturating_sub(start.elapsed().as_millis() as u64);
        if request.timeout_ms == 0 {
            return Err(invalid("agent deadline exhausted during preflight"));
        }
        let result = self
            .connector
            .execute(&request)
            .map_err(|error| invalid(error.to_string()))?;
        if result.task_id != request.id || result.agent != self.info.name {
            return Err(invalid("agent result identity does not match invocation"));
        }
        if result.success
            && (result.exit_code != 0
                || matches!(
                    result.termination,
                    Some(TaskTermination::TimedOut | TaskTermination::Signalled)
                ))
        {
            return Err(invalid(
                "agent result contains contradictory terminal state",
            ));
        }
        // Common counts measure local payloads. Provider facts remain optional
        // and separately named; no unknown charge becomes a zero-cost claim.
        let input_tokens = crate::core::tokens::count_tokens(&request.prompt) as u64;
        let output_tokens = crate::core::tokens::count_tokens(&result.stdout) as u64;
        let latency_ms = start.elapsed().as_millis() as u64;
        if result.success && latency_ms > deadline_ms {
            return Err(invalid("agent invocation exceeded its admitted deadline"));
        }
        let mut observation = CapabilityObservationV1::success(
            invocation,
            input_tokens,
            output_tokens,
            latency_ms,
            Some(evidence_ref(&result.stdout)),
        );
        observation.success = result.success;
        observation.failure_mode = if result.success {
            None
        } else {
            Some(match result.termination {
                Some(TaskTermination::TimedOut) => CapabilityFailureMode::Timeout,
                _ => CapabilityFailureMode::Internal,
            })
        };
        observation
            .metrics
            .insert("local_payload_token_measurement".into(), 1);
        if let Some(usage) = result.tokens_used {
            observation
                .metrics
                .insert("provider_reported_input_tokens".into(), usage.input_tokens);
            observation.metrics.insert(
                "provider_reported_output_tokens".into(),
                usage.output_tokens,
            );
        }
        if let Some(cost) = result.provider_cost_micros {
            observation
                .metrics
                .insert("provider_reported_cost_micros".into(), cost);
        }
        let common = CapabilityResult {
            success: result.success,
            output_tokens,
            latency_ms,
            observation,
            // Preserve the existing unverified-provider/local-signature linkage.
            // Process success is not task-quality acceptance or provider attestation.
            evidence_ref: result.execution_receipt_ref.clone(),
        };
        Ok((result, common))
    }

    fn runtime_unchanged(&self) -> bool {
        self.info.available
            && self.connector.info() == self.info
            && self.connector.supports_model_selection() == self.model_selection
            && self.connector.supports_turn_limit() == self.turn_limit
    }
}

impl CapabilityAdapter for AgentConnectorAdapter {
    fn manifest(&self) -> &CapabilityManifestV1 {
        &self.manifest
    }

    fn invoke(&self, invocation: CapabilityInvocation) -> OclaResult<CapabilityResult> {
        self.execute_checked(&invocation).map(|(_, common)| common)
    }

    fn health_check(&self) -> OclaResult<bool> {
        Ok(self.runtime_unchanged() && self.connector.health_check().unwrap_or(false))
    }
}

#[cfg(all(test, unix))]
mod tests;
