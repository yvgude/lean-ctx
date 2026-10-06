//! Common invocation and measurement envelopes for OCLA capability adapters.

use std::collections::BTreeMap;

use lean_ctx_protocol::CapabilityManifestV1;
use serde::{Deserialize, Serialize};

use super::{OclaError, OclaResult};

pub use crate::core::agent_connector::traits::TaskRequest as AgentTaskRequest;

/// Version of the public observation shape emitted by native adapters.
pub const CAPABILITY_OBSERVATION_SCHEMA_VERSION: u32 = 1;

/// Input accepted by a capability adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityInput {
    ContextRequest {
        paths: Vec<String>,
        mode: String,
        budget_tokens: Option<u64>,
    },
    ShellCommand {
        command: String,
        workdir: Option<String>,
    },
    ModelRequest {
        prompt: String,
        model: Option<String>,
    },
    AgentTask(AgentTaskRequest),
}

impl CapabilityInput {
    /// Returns the payload measured by the common token envelope.
    #[must_use]
    pub fn payload(&self) -> &str {
        match self {
            Self::ContextRequest { paths, .. } => paths.first().map(String::as_str).unwrap_or(""),
            Self::ShellCommand { command, .. } => command,
            Self::ModelRequest { prompt, .. } => prompt,
            Self::AgentTask(request) => &request.prompt,
        }
    }
}

/// Policy limits applied uniformly before and after adapter execution.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyConstraints {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_latency_ms: Option<u64>,
    pub allowed_models: Vec<String>,
    pub allowed_paths: Vec<String>,
    pub allowed_data_classifications: Vec<String>,
    pub allow_remote: bool,
    pub require_deterministic: bool,
    pub require_reversible: bool,
}

impl PolicyConstraints {
    /// Validate internal policy invariants.
    pub fn validate(&self) -> OclaResult<()> {
        if self
            .allowed_models
            .iter()
            .chain(self.allowed_paths.iter())
            .chain(self.allowed_data_classifications.iter())
            .any(|value| value.trim().is_empty())
        {
            return Err(OclaError::InvalidRequest(
                "policy constraint lists must not contain empty values".into(),
            ));
        }
        Ok(())
    }

    /// Apply the model/path portions of policy to an invocation.
    pub fn check_input(&self, input: &CapabilityInput) -> OclaResult<()> {
        self.validate()?;
        match input {
            CapabilityInput::ContextRequest { paths, .. } => {
                if !self.allowed_paths.is_empty()
                    && paths
                        .iter()
                        .any(|path| !self.allowed_paths.iter().any(|allowed| path == allowed))
                {
                    return Err(OclaError::InvalidRequest(
                        "context path is outside policy allowlist".into(),
                    ));
                }
            }
            CapabilityInput::ModelRequest { model, .. }
            | CapabilityInput::AgentTask(AgentTaskRequest { model, .. }) => {
                if !self.allowed_models.is_empty()
                    && model.as_ref().is_none_or(|model| {
                        !self.allowed_models.iter().any(|allowed| allowed == model)
                    })
                {
                    return Err(OclaError::InvalidRequest(
                        "model is outside policy allowlist".into(),
                    ));
                }
            }
            CapabilityInput::ShellCommand { .. } => {}
        }
        if let CapabilityInput::AgentTask(request) = input
            && !self.allowed_paths.is_empty()
            && !self
                .allowed_paths
                .iter()
                .any(|path| std::path::Path::new(path) == request.working_dir)
        {
            return Err(OclaError::InvalidRequest(
                "agent working directory is outside policy allowlist".into(),
            ));
        }
        Ok(())
    }
}

/// A single capability invocation with task and policy lineage attached.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityInvocation {
    pub task_id: String,
    pub capability_id: String,
    pub capability_version: String,
    pub input: CapabilityInput,
    pub policy_constraints: PolicyConstraints,
    pub timeout_ms: u64,
}

impl CapabilityInvocation {
    /// Validate identity and policy fields before dispatch.
    pub fn validate(&self) -> OclaResult<()> {
        for (label, value) in [
            ("task_id", &self.task_id),
            ("capability_id", &self.capability_id),
            ("capability_version", &self.capability_version),
        ] {
            if value.trim().is_empty() {
                return Err(OclaError::InvalidRequest(format!("{label} is required")));
            }
        }
        self.policy_constraints.check_input(&self.input)
    }
}

/// Failure taxonomy shared by all adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityFailureMode {
    Timeout,
    Unavailable,
    RejectedByPolicy,
    InvalidOutput,
    Partial,
    FallbackToNative,
    Internal,
}

/// Comparable, payload-free measurement emitted for one invocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityObservationV1 {
    pub schema_version: u32,
    pub task_id: String,
    pub capability_id: String,
    pub capability_version: String,
    pub success: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u64,
    pub failure_mode: Option<CapabilityFailureMode>,
    pub output_ref: Option<String>,
    pub metrics: BTreeMap<String, u64>,
}

impl CapabilityObservationV1 {
    /// Build the successful observation shared by native adapters.
    #[must_use]
    pub fn success(
        invocation: &CapabilityInvocation,
        input_tokens: u64,
        output_tokens: u64,
        latency_ms: u64,
        output_ref: Option<String>,
    ) -> Self {
        Self {
            schema_version: CAPABILITY_OBSERVATION_SCHEMA_VERSION,
            task_id: invocation.task_id.clone(),
            capability_id: invocation.capability_id.clone(),
            capability_version: invocation.capability_version.clone(),
            success: true,
            input_tokens,
            output_tokens,
            latency_ms,
            failure_mode: None,
            output_ref,
            metrics: BTreeMap::new(),
        }
    }
}

/// Result returned by every adapter through the common invocation path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityResult {
    pub success: bool,
    pub output_tokens: u64,
    pub latency_ms: u64,
    pub observation: CapabilityObservationV1,
    pub evidence_ref: Option<String>,
}

/// Adapter boundary shared by native, reference, and enterprise capabilities.
pub trait CapabilityAdapter: Send + Sync {
    fn manifest(&self) -> &CapabilityManifestV1;
    fn invoke(&self, invocation: CapabilityInvocation) -> OclaResult<CapabilityResult>;
    fn health_check(&self) -> OclaResult<bool>;
}

/// Create a stable content-addressed evidence reference without exposing data.
#[must_use]
pub(crate) fn evidence_ref(content: &str) -> String {
    format!("blake3:{}", blake3::hash(content.as_bytes()).to_hex())
}

/// Enforce the invocation timeout after a synchronous adapter operation.
pub(crate) fn check_timeout(start: std::time::Instant, timeout_ms: u64) -> OclaResult<u64> {
    let elapsed_ms = start.elapsed().as_millis() as u64;
    if timeout_ms != 0 && elapsed_ms > timeout_ms {
        return Err(OclaError::InvalidRequest(format!(
            "capability invocation exceeded timeout ({elapsed_ms}ms > {timeout_ms}ms)"
        )));
    }
    Ok(elapsed_ms)
}
