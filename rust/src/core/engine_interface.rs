// SPDX-License-Identifier: Apache-2.0

//! Internal proof bridge from one native OCLA capability to Engine Interface v1.
//!
//! This module is deliberately crate-private. It proves the local Engine
//! contract without promoting an SDK façade, an agent loop, or Cloud semantics.

use std::path::{Path, PathBuf};

pub(crate) mod context_plan;
pub(crate) mod planning;
pub(crate) mod source_execution;

use lean_ctx_protocol::{
    CapabilityId, EngineFailureCodeV1, EngineFailureV1, EngineInterfaceV1, EngineInvocationIdV1,
    EngineInvocationV1, EngineMeasurementV1, EngineObservationStatusV1, EngineObservationV1,
    EngineOperationV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1, EngineReceiptLinkV1,
    EngineValueClassificationV1, ProtocolReference, ReceiptId, ResolvedLocalEngineIdentityV1,
    SemanticVersion, Sha256Digest, V1_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::canonical;
use crate::core::ocla::adapters::native_context::{
    NativeContextAdapter, NativeContextInvocationFailure,
};
use crate::core::ocla::invocation::{CapabilityInput, CapabilityInvocation, PolicyConstraints};

const ENGINE_ID: &str = "lean-ctx-local";
pub(crate) const CAPABILITY_ID: &str = "capability://leanctx/context-optimization";
pub(crate) const CAPABILITY_VERSION: &str = "1.0.0";
const RECEIPT_DIRECTORY: &str = "engine-interface/v1/receipts";
const OUTPUT_DIRECTORY: &str = "engine-interface/v1/outputs";
const RECOVERY_DIRECTORY: &str = "engine-interface/v1/recovery";
pub(crate) const ENGINE_TRANSPORT_VERSION: u32 = 1;
pub(crate) const ENGINE_INTERFACE_VERSION: &str = "1.0.0";
pub(crate) const ENGINE_TRANSPORT_POLICY_REF: &str = "policy:engine-transport-v1:admitted";
const INPUT_REF_PREFIX: &str = "input:ctx-read-snapshot-sha256:";
const SOURCE_REF_PREFIX: &str = "source:canonical-path-sha256:";
const MAX_TRANSPORT_VIEW_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_TRANSPORT_BUDGET_TOKENS: u64 = MAX_TRANSPORT_VIEW_BYTES as u64;

use super::engine_artifact as artifact_store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngineTransportError {
    InvalidRequest,
    UnsupportedMode,
    UnsafeRoot,
    SourceOutsideRoot,
    SourceSymlink,
    SourceUnavailable,
    UnsupportedInput,
    SourceChanged,
    MalformedRecoveryRef,
    ReceiptMismatch,
    ObservationMismatch,
    ViewTooLarge,
    Internal,
}

impl EngineTransportError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::UnsupportedMode => "unsupported_mode",
            Self::UnsafeRoot => "unsafe_root",
            Self::SourceOutsideRoot => "source_outside_root",
            Self::SourceSymlink => "source_symlink",
            Self::SourceUnavailable => "source_unavailable",
            Self::UnsupportedInput => "unsupported_input",
            Self::SourceChanged => "source_changed",
            Self::MalformedRecoveryRef => "malformed_recovery_ref",
            Self::ReceiptMismatch => "receipt_mismatch",
            Self::ObservationMismatch => "observation_mismatch",
            Self::ViewTooLarge => "view_too_large",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EngineTransportRecoveryDescriptor {
    pub(crate) recovery_ref: ProtocolReference,
    pub(crate) source_ref: ProtocolReference,
    pub(crate) source_digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EngineTransportView {
    pub(crate) text: String,
    pub(crate) output_ref: Option<ProtocolReference>,
    pub(crate) output_digest: Option<Sha256Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EngineTransportResult {
    pub(crate) view: EngineTransportView,
    pub(crate) invocation: Option<EngineInvocationV1>,
    pub(crate) observation: Option<EngineObservationV1>,
    pub(crate) recovery: EngineTransportRecoveryDescriptor,
}

pub(crate) fn persist_engine_artifact_content(
    directory: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<std::fs::File, String> {
    artifact_store::persist_content(directory, digest, extension, bytes)
}

#[cfg(windows)]
pub(crate) fn open_engine_artifact_lock(
    directory: &str,
    digest: &str,
) -> Result<std::fs::File, String> {
    artifact_store::open_lock(directory, digest)
}

/// Inputs intentionally retained inside the local Engine proof boundary.
///
/// The caller provides source and input identities before dispatch. The bridge
/// verifies the expected input digest against the bytes read through the native
/// rooted adapter, so an observation can never claim a different source input.
#[derive(Clone, Debug)]
struct NativeContextEngineRequest {
    invocation_id: EngineInvocationIdV1,
    input_ref: ProtocolReference,
    input_digest: Sha256Digest,
    source_refs: Vec<ProtocolReference>,
    policy_admission: EnginePolicyAdmissionV1,
    paths: Vec<String>,
    mode: String,
    budget_tokens: Option<u64>,
    timeout_ms: u64,
}

impl NativeContextEngineRequest {
    fn ctx_read_snapshot(
        path: &str,
        raw_input: &str,
        timeout_ms: u64,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<(Self, String), String> {
        let canonical_path = std::fs::canonicalize(path)
            .map_err(|error| format!("resolve ctx_read Engine source: {error}"))?;
        Self::ctx_read_snapshot_canonical(&canonical_path, raw_input, timeout_ms, policy_admission)
    }

    fn ctx_read_snapshot_canonical(
        canonical_path: &Path,
        raw_input: &str,
        timeout_ms: u64,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<(Self, String), String> {
        if !canonical_path.is_absolute() {
            return Err("ctx_read Engine canonical source must be absolute".to_owned());
        }
        Self::ctx_read_identified(
            canonical_path.to_string_lossy().into_owned(),
            "source:canonical-path-sha256",
            raw_input,
            timeout_ms,
            policy_admission,
        )
    }

    fn ctx_read_rejection(
        requested_path: &str,
        timeout_ms: u64,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<Self, String> {
        if !Path::new(requested_path).is_absolute() {
            return Err("ctx_read Engine requested source must be absolute".to_owned());
        }
        Self::ctx_read_identified(
            requested_path.to_owned(),
            "source:requested-path-sha256",
            "",
            timeout_ms,
            policy_admission,
        )
        .map(|(request, _)| request)
    }

    fn ctx_read_identified(
        path_identity: String,
        source_prefix: &str,
        raw_input: &str,
        timeout_ms: u64,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<(Self, String), String> {
        let input = crate::core::redaction::redact_text_if_enabled(raw_input);
        let raw_input_digest = sha256_digest(raw_input.as_bytes())?;
        let input_digest = sha256_digest(input.as_bytes())?;
        let input_ref = ProtocolReference::new(format!(
            "input:ctx-read-snapshot-sha256:{}",
            raw_input_digest.hex()
        ))
        .map_err(|error| error.to_string())?;
        let path_digest = sha256_digest(path_identity.as_bytes())?;
        let source_ref = ProtocolReference::new(format!("{source_prefix}:{}", path_digest.hex()))
            .map_err(|error| error.to_string())?;
        let invocation_seed = canonical::canonical_serialize(&CtxReadInvocationIdentityV1 {
            engine_id: ENGINE_ID,
            engine_version: env!("CARGO_PKG_VERSION"),
            capability_id: CAPABILITY_ID,
            capability_version: CAPABILITY_VERSION,
            input_ref: input_ref.as_str(),
            source_ref: source_ref.as_str(),
            input_digest: input_digest.as_str(),
            policy_ref: policy_admission.policy_ref.as_str(),
            policy_decision: policy_admission.decision,
            mode: "aggressive",
            timeout_ms,
        });
        let invocation_digest = sha256_digest(&invocation_seed)?;
        let request = Self {
            invocation_id: EngineInvocationIdV1::new(format!(
                "engine-invocation-{}",
                &invocation_digest.hex()[..32]
            ))
            .map_err(|error| error.to_string())?,
            input_ref: input_ref.clone(),
            input_digest,
            source_refs: vec![input_ref, source_ref],
            policy_admission,
            paths: vec![path_identity],
            mode: "aggressive".to_owned(),
            budget_tokens: None,
            timeout_ms,
        };
        Ok((request, input))
    }
}

#[derive(Serialize)]
struct CtxReadInvocationIdentityV1<'a> {
    engine_id: &'a str,
    engine_version: &'a str,
    capability_id: &'a str,
    capability_version: &'a str,
    input_ref: &'a str,
    source_ref: &'a str,
    input_digest: &'a str,
    policy_ref: &'a str,
    policy_decision: EnginePolicyDecisionV1,
    mode: &'a str,
    timeout_ms: u64,
}

/// One strictly local implementation of the published Engine records.
pub(crate) struct NativeContextEngine {
    adapter: NativeContextAdapter,
}

/// Stored without a receipt link so its digest cannot self-reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EngineReceiptArtifactV1 {
    schema_version: u32,
    invocation: EngineInvocationV1,
    observation: EngineObservationV1,
}

/// Receipt contents returned only after the on-disk artifact and both expected
/// records have passed the local verification boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedEngineReceiptV1 {
    artifact: EngineReceiptArtifactV1,
    digest: Sha256Digest,
}

impl VerifiedEngineReceiptV1 {
    pub(crate) fn invocation(&self) -> &EngineInvocationV1 {
        &self.artifact.invocation
    }

    pub(crate) fn observation(&self) -> &EngineObservationV1 {
        &self.artifact.observation
    }

    pub(crate) fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

/// Build the exact canonical bytes persisted for one Engine receipt.
pub(crate) fn canonical_engine_receipt_artifact_bytes(
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
) -> Vec<u8> {
    canonical::canonical_serialize(&EngineReceiptArtifactV1 {
        schema_version: V1_SCHEMA_VERSION,
        invocation: invocation.clone(),
        observation: observation.clone(),
    })
}

/// Read and verify one Engine receipt artifact against its expected records.
pub(crate) fn read_verified_engine_receipt(
    digest: &Sha256Digest,
    expected_invocation: &EngineInvocationV1,
    expected_observation_without_link: &EngineObservationV1,
) -> Result<VerifiedEngineReceiptV1, String> {
    if expected_observation_without_link.receipt_link.is_some() {
        return Err("expected Engine observation must omit receipt_link".to_owned());
    }
    expected_invocation
        .validate()
        .map_err(|error| format!("invalid expected Engine invocation: {error}"))?;
    expected_observation_without_link
        .validate_for(expected_invocation)
        .map_err(|error| format!("invalid expected Engine observation: {error}"))?;

    let bytes = artifact_store::read_content(RECEIPT_DIRECTORY, digest.hex(), "json")?;
    let actual_digest = sha256_digest(&bytes)?;
    if actual_digest != *digest {
        return Err("Engine receipt artifact digest mismatch".to_owned());
    }
    let artifact = decode_canonical_engine_receipt_artifact(&bytes)?;
    if artifact.schema_version != V1_SCHEMA_VERSION {
        return Err("Engine receipt artifact schema_version must be 1".to_owned());
    }
    if artifact.observation.receipt_link.is_some() {
        return Err("Engine receipt artifact must omit receipt_link".to_owned());
    }
    artifact
        .invocation
        .validate()
        .map_err(|error| format!("invalid Engine receipt invocation: {error}"))?;
    artifact
        .observation
        .validate_for(&artifact.invocation)
        .map_err(|error| format!("invalid Engine receipt observation: {error}"))?;
    if artifact.invocation != *expected_invocation {
        return Err("Engine receipt invocation does not match expectation".to_owned());
    }
    if artifact.observation != *expected_observation_without_link {
        return Err("Engine receipt observation does not match expectation".to_owned());
    }
    Ok(VerifiedEngineReceiptV1 {
        artifact,
        digest: digest.clone(),
    })
}

fn decode_canonical_engine_receipt_artifact(
    bytes: &[u8],
) -> Result<EngineReceiptArtifactV1, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let artifact = EngineReceiptArtifactV1::deserialize(&mut deserializer)
        .map_err(|error| format!("decode Engine receipt artifact: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("trailing Engine receipt artifact data: {error}"))?;
    if canonical_engine_receipt_artifact_bytes(&artifact.invocation, &artifact.observation) != bytes
    {
        return Err("Engine receipt artifact is not canonical JSON".to_owned());
    }
    Ok(artifact)
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct EngineRecoveryArtifactV1 {
    schema_version: u32,
    invocation: EngineInvocationV1,
    observation: EngineObservationV1,
    failure_class: &'static str,
}

impl NativeContextEngine {
    pub(crate) fn with_root(root: impl AsRef<Path>) -> Result<Self, String> {
        let root = crate::core::pathutil::canonicalize_secure(root.as_ref())
            .map_err(|_| "ctx_read Engine root cannot be bound securely".to_owned())?;
        Ok(Self {
            adapter: NativeContextAdapter::with_root(root),
        })
    }

    fn interface(&self) -> Result<EngineInterfaceV1, String> {
        let interface = EngineInterfaceV1 {
            schema_version: V1_SCHEMA_VERSION,
            interface_version: SemanticVersion::new("1.0.0").map_err(|error| error.to_string())?,
            engine: engine_identity()?,
            supported_operations: vec![native_operation()?],
        };
        interface.validate().map_err(|error| error.to_string())?;
        Ok(interface)
    }

    /// Execute the one admitted local capability and return its factual record.
    ///
    /// Rejections never enter the adapter. Successful output is persisted only
    /// as a local integrity-addressed artifact; the observation exposes its
    /// reference and SHA-256, never the payload.
    fn execute(
        &self,
        request: NativeContextEngineRequest,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        self.execute_inner(request, None)
    }

    /// Execute against a caller-owned immutable source snapshot.
    ///
    /// The digest is still verified by the adapter; the only difference from
    /// `execute` is that no second disk read can race the production caller.
    fn execute_materialized(
        &self,
        request: NativeContextEngineRequest,
        input: &str,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        self.execute_inner(request, Some(input))
    }

    /// Bind and execute the production `ctx_read` snapshot as one operation.
    pub(crate) fn execute_ctx_read_snapshot(
        &self,
        path: &str,
        raw_input: &str,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        let rooted_path = crate::core::pathjail::jail_path(Path::new(path), self.adapter.root())
            .map_err(|_| "ctx_read Engine source is outside its rooted boundary".to_owned())?;
        self.execute_ctx_read_rooted_snapshot(
            &rooted_path.to_string_lossy(),
            raw_input,
            policy_admission,
        )
    }

    pub(crate) fn execute_ctx_read_rooted_snapshot(
        &self,
        rooted_path: &str,
        raw_input: &str,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        self.execute_ctx_read_rooted_snapshot_with_budget(
            rooted_path,
            raw_input,
            policy_admission,
            None,
            None,
        )
    }

    pub(crate) fn execute_ctx_read_rooted_snapshot_with_plan(
        &self,
        rooted_path: &str,
        raw_input: &str,
        policy_admission: EnginePolicyAdmissionV1,
        task: &lean_ctx_protocol::TaskEnvelopeV1,
        plan: &lean_ctx_protocol::ExecutionPlanV1,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        planning::validate_native_plan(task, plan, &policy_admission)?;
        self.execute_ctx_read_rooted_snapshot_with_budget(
            rooted_path,
            raw_input,
            policy_admission,
            plan.context_token_limit(),
            Some((task, plan)),
        )
    }

    fn execute_ctx_read_rooted_snapshot_with_budget(
        &self,
        rooted_path: &str,
        raw_input: &str,
        policy_admission: EnginePolicyAdmissionV1,
        budget_tokens: Option<u64>,
        binding: Option<(
            &lean_ctx_protocol::TaskEnvelopeV1,
            &lean_ctx_protocol::ExecutionPlanV1,
        )>,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        let root = self.adapter.root();
        let rooted_path = Path::new(rooted_path);
        if !rooted_path.is_absolute() {
            return Err("ctx_read Engine source is outside its rooted boundary".to_owned());
        }
        if contains_symlink_component(rooted_path, root) {
            return Err("ctx_read Engine source is outside its rooted boundary".to_owned());
        }
        // The Engine binds its root through `canonicalize_secure` before building
        // the adapter. Bring caller-supplied paths into that same form before
        // comparing: on Windows std::fs::canonicalize may return a `\\?\` path, while pathjail and
        // canonicalize_secure strip that prefix. Resolving here also collapses
        // short-name, case, and reparse-point aliases before the component-boundary
        // check; it does not broaden the root.
        let rooted_path = crate::core::pathutil::canonicalize_secure(rooted_path)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    unresolved_rooted_path(rooted_path, root).ok_or(error)
                } else {
                    Err(error)
                }
            })
            .map_err(|_| "ctx_read Engine source is outside its rooted boundary".to_owned())?;
        if !rooted_path.starts_with(root) {
            return Err("ctx_read Engine source is outside its rooted boundary".to_owned());
        }
        let (mut request, input) = NativeContextEngineRequest::ctx_read_snapshot_canonical(
            &rooted_path,
            raw_input,
            30_000,
            policy_admission,
        )?;
        if let Some(budget) = budget_tokens {
            if budget == 0 {
                return Err("ctx_read Engine budget must be positive".to_owned());
            }
            let identity = sha256_digest(&canonical::canonical_serialize(&(
                "ctx-read-budget-v1",
                &request.invocation_id,
                budget,
            )))?;
            request.invocation_id =
                EngineInvocationIdV1::new(format!("engine-invocation-{}", &identity.hex()[..32]))
                    .map_err(|error| error.to_string())?;
            request.budget_tokens = Some(budget);
        }
        if let Some((task, plan)) = binding {
            // Persist and bind the admitted documents before the adapter can execute.
            planning::persist_evidence(task)?;
            planning::persist_evidence(plan)?;
            let refs = planning::binding_refs(task, plan)?;
            let identity = sha256_digest(&canonical::canonical_serialize(&(
                "ctx-read-plan-v1",
                &request.invocation_id,
                &refs,
            )))?;
            request.invocation_id =
                EngineInvocationIdV1::new(format!("engine-invocation-{}", &identity.hex()[..32]))
                    .map_err(|error| error.to_string())?;
            request.source_refs.extend(refs);
        }
        self.execute_materialized(request, &input)
    }

    pub(crate) fn execute_ctx_read_rejection(
        &self,
        path: &str,
        policy_admission: EnginePolicyAdmissionV1,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        if policy_admission.decision != EnginePolicyDecisionV1::Rejected {
            return Err("ctx_read rejection requires a rejected policy admission".to_owned());
        }
        let request =
            NativeContextEngineRequest::ctx_read_rejection(path, 30_000, policy_admission)?;
        self.execute(request)
    }

    fn execute_inner(
        &self,
        request: NativeContextEngineRequest,
        materialized_input: Option<&str>,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        let invocation = self.invocation_for(&request)?;
        if request.policy_admission.decision == EnginePolicyDecisionV1::Rejected {
            let observation = EngineObservationV1 {
                schema_version: V1_SCHEMA_VERSION,
                invocation_id: invocation.invocation_id.clone(),
                status: EngineObservationStatusV1::Rejected,
                output_ref: None,
                output_digest: None,
                source_lineage: invocation.source_refs.clone(),
                measurements: Vec::new(),
                failure: Some(EngineFailureV1 {
                    code: EngineFailureCodeV1::PolicyRejected,
                    retryable_by_host: false,
                    recovery_ref: None,
                }),
                receipt_link: None,
            };
            return self.persist_terminal(invocation, observation);
        }

        let native_invocation = CapabilityInvocation {
            task_id: request.invocation_id.as_str().to_owned(),
            capability_id: CAPABILITY_ID.to_owned(),
            capability_version: CAPABILITY_VERSION.to_owned(),
            input: CapabilityInput::ContextRequest {
                paths: request.paths,
                mode: request.mode,
                budget_tokens: request.budget_tokens,
            },
            policy_constraints: PolicyConstraints::default(),
            timeout_ms: request.timeout_ms,
        };

        let native_result = match materialized_input {
            Some(input) => self
                .adapter
                .invoke_materialized_bounded(native_invocation, input.to_owned()),
            None => self.adapter.invoke_with_output_identity(&native_invocation),
        };
        let observation = match native_result {
            Ok(result) if result.input_digest == request.input_digest.as_str() => {
                let output_digest = Sha256Digest::new(result.output_digest)
                    .map_err(|error| format!("native output digest is invalid: {error}"))?;
                let output_ref = ProtocolReference::new(format!("output:{}", output_digest.hex()))
                    .map_err(|error| error.to_string())?;
                match persist_output(output_digest.hex(), &result.output) {
                    Ok(()) => EngineObservationV1 {
                        schema_version: V1_SCHEMA_VERSION,
                        invocation_id: invocation.invocation_id.clone(),
                        status: EngineObservationStatusV1::Succeeded,
                        output_ref: Some(output_ref),
                        output_digest: Some(output_digest),
                        source_lineage: invocation.source_refs.clone(),
                        measurements: measured_observations(&result.result),
                        failure: None,
                        receipt_link: None,
                    },
                    Err(error) => {
                        tracing::warn!(%error, "failed to persist Engine output artifact");
                        storage_failure_observation(&invocation)
                    }
                }
            }
            Ok(_) => source_integrity_failure(&invocation),
            Err(failure) => adapter_failure(&invocation, &failure),
        };
        self.persist_terminal(invocation, observation)
    }

    fn invocation_for(
        &self,
        request: &NativeContextEngineRequest,
    ) -> Result<EngineInvocationV1, String> {
        let invocation = EngineInvocationV1 {
            schema_version: V1_SCHEMA_VERSION,
            invocation_id: request.invocation_id.clone(),
            engine: engine_identity()?,
            operation: native_operation()?,
            input_ref: request.input_ref.clone(),
            input_digest: request.input_digest.clone(),
            source_refs: request.source_refs.clone(),
            policy_admission: request.policy_admission.clone(),
        };
        invocation.validate().map_err(|error| error.to_string())?;
        Ok(invocation)
    }

    fn persist_terminal(
        &self,
        invocation: EngineInvocationV1,
        observation: EngineObservationV1,
    ) -> Result<(EngineInvocationV1, EngineObservationV1), String> {
        observation
            .validate_for(&invocation)
            .map_err(|error| format!("invalid Engine observation: {error}"))?;
        let receipt_link = match persist_receipt(&invocation, &observation) {
            Ok(receipt_link) => receipt_link,
            Err(error) => {
                let recovery_ref = persist_recovery(
                    &invocation,
                    &observation,
                    "receipt-persistence-failed",
                )
                .map_err(|recovery_error| {
                    format!(
                        "persist Engine receipt: {error}; persist Engine recovery artifact: {recovery_error}"
                    )
                })?;
                return Err(format!(
                    "persist Engine receipt: {error}; recovery_ref={}",
                    recovery_ref.as_str()
                ));
            }
        };
        let mut observation = observation;
        observation.receipt_link = Some(receipt_link);
        observation
            .validate_for(&invocation)
            .map_err(|error| format!("invalid Engine observation receipt link: {error}"))?;
        Ok((invocation, observation))
    }
}

/// Execute the standalone, versioned local Engine transport operation.
pub(crate) fn execute_transport_context_view(
    root: &Path,
    path: &str,
) -> Result<EngineTransportResult, EngineTransportError> {
    execute_transport_context_view_with_budget(root, path, None)
}

pub(crate) fn execute_transport_context_view_with_budget(
    root: &Path,
    path: &str,
    budget_tokens: Option<u64>,
) -> Result<EngineTransportResult, EngineTransportError> {
    execute_transport_context_view_bound(root, path, budget_tokens, None)
}

pub(crate) fn execute_transport_context_view_with_plan(
    root: &Path,
    path: &str,
    task: &lean_ctx_protocol::TaskEnvelopeV1,
    plan: &lean_ctx_protocol::ExecutionPlanV1,
) -> Result<EngineTransportResult, EngineTransportError> {
    execute_transport_context_view_bound(root, path, plan.context_token_limit(), Some((task, plan)))
}

fn execute_transport_context_view_bound(
    root: &Path,
    path: &str,
    budget_tokens: Option<u64>,
    binding: Option<(
        &lean_ctx_protocol::TaskEnvelopeV1,
        &lean_ctx_protocol::ExecutionPlanV1,
    )>,
) -> Result<EngineTransportResult, EngineTransportError> {
    if budget_tokens.is_some_and(|budget| budget == 0 || budget > MAX_TRANSPORT_BUDGET_TOKENS) {
        return Err(EngineTransportError::InvalidRequest);
    }
    let root = bind_transport_root(root)?;
    let (input, canonical_path) = read_transport_source(&root, path)?;
    if input.len() > MAX_TRANSPORT_VIEW_BYTES {
        return Err(EngineTransportError::ViewTooLarge);
    }

    let source_digest =
        sha256_digest(input.as_bytes()).map_err(|_| EngineTransportError::Internal)?;
    let source_ref = source_reference(&canonical_path)?;
    let policy_admission = EnginePolicyAdmissionV1 {
        policy_ref: ProtocolReference::new(ENGINE_TRANSPORT_POLICY_REF)
            .map_err(|_| EngineTransportError::Internal)?,
        decision: EnginePolicyDecisionV1::Admitted,
    };
    let engine =
        NativeContextEngine::with_root(&root).map_err(|_| EngineTransportError::UnsafeRoot)?;
    if let Some((task, plan)) = binding {
        planning::validate_native_plan(task, plan, &policy_admission)
            .map_err(|_| EngineTransportError::InvalidRequest)?;
    }
    let (invocation, observation) = engine
        .execute_ctx_read_rooted_snapshot_with_budget(
            &canonical_path,
            &input,
            policy_admission,
            budget_tokens,
            binding,
        )
        .map_err(|_| EngineTransportError::Internal)?;

    let recovered_digest = parse_input_recovery_ref(&invocation.input_ref)?;
    if recovered_digest != source_digest {
        return Err(EngineTransportError::ObservationMismatch);
    }
    if !invocation.source_refs.contains(&source_ref) {
        return Err(EngineTransportError::ObservationMismatch);
    }

    let view = verified_output_view(&invocation, &observation)?;
    let recovery_ref = invocation.input_ref.clone();
    Ok(EngineTransportResult {
        view,
        invocation: Some(invocation),
        observation: Some(observation),
        recovery: EngineTransportRecoveryDescriptor {
            recovery_ref,
            source_ref,
            source_digest,
        },
    })
}

/// Read only the content-addressed output bound by the verified terminal receipt.
pub(crate) fn verified_output_view(
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
) -> Result<EngineTransportView, EngineTransportError> {
    if observation.status != EngineObservationStatusV1::Succeeded {
        return Err(EngineTransportError::ObservationMismatch);
    }
    let receipt_link = observation
        .receipt_link
        .as_ref()
        .ok_or(EngineTransportError::ReceiptMismatch)?;
    if receipt_link.receipt_ref.as_str()
        != format!("receipt:sha256:{}", receipt_link.receipt_digest.hex())
    {
        return Err(EngineTransportError::ReceiptMismatch);
    }
    let mut expected_observation = observation.clone();
    expected_observation.receipt_link = None;
    read_verified_engine_receipt(
        &receipt_link.receipt_digest,
        invocation,
        &expected_observation,
    )
    .map_err(|_| EngineTransportError::ReceiptMismatch)?;

    let output_digest = observation
        .output_digest
        .clone()
        .ok_or(EngineTransportError::ObservationMismatch)?;
    let expected_output_ref = format!("output:{}", output_digest.hex());
    if observation
        .output_ref
        .as_ref()
        .map(ProtocolReference::as_str)
        != Some(expected_output_ref.as_str())
    {
        return Err(EngineTransportError::ObservationMismatch);
    }
    let output = artifact_store::read_content(OUTPUT_DIRECTORY, output_digest.hex(), "txt")
        .map_err(|_| EngineTransportError::Internal)?;
    if output.len() > MAX_TRANSPORT_VIEW_BYTES {
        return Err(EngineTransportError::ViewTooLarge);
    }
    let output_text =
        String::from_utf8(output).map_err(|_| EngineTransportError::UnsupportedInput)?;
    Ok(EngineTransportView {
        text: output_text,
        output_ref: observation.output_ref.clone(),
        output_digest: Some(output_digest),
    })
}

/// Re-read the exact source snapshot named by a prior Engine input reference.
pub(crate) fn recover_transport_source(
    root: &Path,
    path: &str,
    recovery_ref: &ProtocolReference,
    source_ref: &ProtocolReference,
    source_digest: &Sha256Digest,
) -> Result<EngineTransportResult, EngineTransportError> {
    let root = bind_transport_root(root)?;
    let expected_digest = parse_input_recovery_ref(recovery_ref)?;
    if &expected_digest != source_digest {
        return Err(EngineTransportError::MalformedRecoveryRef);
    }

    let (input, canonical_path) = read_transport_source(&root, path)?;
    let actual_source_ref = source_reference(&canonical_path)?;
    if &actual_source_ref != source_ref {
        return Err(EngineTransportError::SourceChanged);
    }
    let actual_digest =
        sha256_digest(input.as_bytes()).map_err(|_| EngineTransportError::Internal)?;
    if &actual_digest != source_digest || actual_digest != expected_digest {
        return Err(EngineTransportError::SourceChanged);
    }
    if input.len() > MAX_TRANSPORT_VIEW_BYTES {
        return Err(EngineTransportError::ViewTooLarge);
    }

    Ok(EngineTransportResult {
        view: EngineTransportView {
            text: input,
            output_ref: None,
            output_digest: Some(actual_digest.clone()),
        },
        invocation: None,
        observation: None,
        recovery: EngineTransportRecoveryDescriptor {
            recovery_ref: recovery_ref.clone(),
            source_ref: source_ref.clone(),
            source_digest: actual_digest,
        },
    })
}

pub(crate) fn bind_transport_root(root: &Path) -> Result<PathBuf, EngineTransportError> {
    if root.as_os_str().is_empty() || crate::core::pathutil::is_broad_or_unsafe_root(root) {
        return Err(EngineTransportError::UnsafeRoot);
    }
    let canonical = crate::core::pathutil::canonicalize_secure(root)
        .map_err(|_| EngineTransportError::UnsafeRoot)?;
    if !canonical.is_dir() || crate::core::pathutil::is_broad_or_unsafe_root(&canonical) {
        return Err(EngineTransportError::UnsafeRoot);
    }
    Ok(canonical)
}

fn read_transport_source(
    root: &Path,
    path: &str,
) -> Result<(String, String), EngineTransportError> {
    if path.trim().is_empty() || path.as_bytes().contains(&0) {
        return Err(EngineTransportError::InvalidRequest);
    }
    let requested = Path::new(path);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let jailed = crate::core::pathjail::jail_path(&candidate, root)
        .map_err(|_| EngineTransportError::SourceOutsideRoot)?;
    // PathJail also admits its allowed extra roots (e.g. the temp dir); the
    // Engine transport is bound to `root` only. Classify "outside" before the
    // symlink walk, which treats any path it cannot place under root as unsafe.
    if relative_path_under_root(&candidate, root).is_none() && !jailed.starts_with(root) {
        return Err(EngineTransportError::SourceOutsideRoot);
    }
    if contains_symlink_component(&candidate, root) {
        return Err(EngineTransportError::SourceSymlink);
    }
    // PathJail validates the caller's spelling. The transport's component walk
    // rejects symlink/reparse points, then the same canonicalizer used to bind
    // the Engine root prevents verbatim prefixes, drive-letter case, and
    // short-name aliases from looking outside the root.
    if !jailed.starts_with(root) {
        return Err(EngineTransportError::SourceOutsideRoot);
    }
    let rooted = crate::tools::ctx_read::read_file_lossy_rooted(
        &jailed.to_string_lossy(),
        &root.to_string_lossy(),
    )
    .map_err(|error| classify_source_error(&error))?;
    let canonical = crate::core::pathutil::canonicalize_secure(Path::new(&rooted.canonical_path))
        .map_err(|_| EngineTransportError::SourceOutsideRoot)?;
    if !canonical.starts_with(root) {
        return Err(EngineTransportError::SourceOutsideRoot);
    }
    Ok((rooted.content, canonical.to_string_lossy().into_owned()))
}

fn classify_source_error(error: &std::io::Error) -> EngineTransportError {
    if error
        .to_string()
        .to_ascii_lowercase()
        .contains("binary file")
    {
        EngineTransportError::UnsupportedInput
    } else {
        EngineTransportError::SourceUnavailable
    }
}

fn contains_symlink_component(path: &Path, root: &Path) -> bool {
    let Some(relative) = relative_path_under_root(path, root) else {
        return true;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !current.pop() {
                    return true;
                }
            }
            std::path::Component::Normal(name) => {
                current.push(name);
                if std::fs::symlink_metadata(&current)
                    .is_ok_and(|metadata| crate::core::pathutil::is_symlink_or_reparse(&metadata))
                {
                    return true;
                }
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => return true,
        }
    }
    false
}

fn relative_path_under_root(path: &Path, root: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative.to_path_buf());
    }

    #[cfg(windows)]
    {
        use std::path::Component;

        // Absolute Windows aliases can spell the same root with a verbatim
        // prefix, separator, or case variation. Compare path components (rather
        // than a string prefix) to retain a safe root boundary, then walk the
        // original root plus the relative components to inspect reparse points.
        let normalized_path =
            crate::core::pathutil::normalize_tool_path_lexical(&path.to_string_lossy())
                .to_lowercase();
        let normalized_root =
            crate::core::pathutil::normalize_tool_path_lexical(&root.to_string_lossy())
                .to_lowercase();
        let path_components: Vec<_> = Path::new(&normalized_path).components().collect();
        let root_components: Vec<_> = Path::new(&normalized_root).components().collect();
        if path_components.len() < root_components.len()
            || !path_components
                .iter()
                .zip(&root_components)
                .all(|(path, root)| path.as_os_str() == root.as_os_str())
        {
            // Short-name aliases do not have the root's lexical component
            // spelling. Resolve them before the reparse walk; the caller still
            // performs the strict canonical component-boundary check.
            let canonical = crate::core::pathutil::canonicalize_secure(path).ok()?;
            if !canonical.starts_with(root) {
                return None;
            }
            return canonical.strip_prefix(root).ok().map(Path::to_path_buf);
        }

        let mut relative = PathBuf::new();
        for component in path_components.into_iter().skip(root_components.len()) {
            match component {
                Component::CurDir => {}
                Component::ParentDir => relative.push(".."),
                Component::Normal(name) => relative.push(name),
                Component::Prefix(_) | Component::RootDir => return None,
            }
        }
        Some(relative)
    }

    #[cfg(not(windows))]
    {
        None
    }
}

fn unresolved_rooted_path(path: &Path, root: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if let Ok(mut rooted) = crate::core::pathutil::canonicalize_secure(ancestor) {
            if !rooted.starts_with(root) {
                return None;
            }
            for component in path.strip_prefix(ancestor).ok()?.components() {
                match component {
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        if rooted == root || !rooted.pop() {
                            return None;
                        }
                    }
                    std::path::Component::Normal(name) => rooted.push(name),
                    std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                        return None;
                    }
                }
            }
            return rooted.starts_with(root).then_some(rooted);
        }
    }
    None
}

fn source_reference(canonical_path: &str) -> Result<ProtocolReference, EngineTransportError> {
    let digest =
        sha256_digest(canonical_path.as_bytes()).map_err(|_| EngineTransportError::Internal)?;
    ProtocolReference::new(format!("{SOURCE_REF_PREFIX}{}", digest.hex()))
        .map_err(|_| EngineTransportError::Internal)
}

fn parse_input_recovery_ref(
    reference: &ProtocolReference,
) -> Result<Sha256Digest, EngineTransportError> {
    let Some(hex) = reference.as_str().strip_prefix(INPUT_REF_PREFIX) else {
        return Err(EngineTransportError::MalformedRecoveryRef);
    };
    Sha256Digest::new(format!("sha256:{hex}"))
        .map_err(|_| EngineTransportError::MalformedRecoveryRef)
}

fn engine_identity() -> Result<ResolvedLocalEngineIdentityV1, String> {
    let identity = ResolvedLocalEngineIdentityV1 {
        engine_id: ENGINE_ID.to_owned(),
        engine_version: SemanticVersion::new(env!("CARGO_PKG_VERSION"))
            .map_err(|error| error.to_string())?,
    };
    identity.validate().map_err(|error| error.to_string())?;
    Ok(identity)
}

fn native_operation() -> Result<EngineOperationV1, String> {
    Ok(EngineOperationV1 {
        capability_id: CapabilityId::new(CAPABILITY_ID).map_err(|error| error.to_string())?,
        capability_version: SemanticVersion::new(CAPABILITY_VERSION)
            .map_err(|error| error.to_string())?,
    })
}

fn measured_observations(
    result: &crate::core::ocla::invocation::CapabilityResult,
) -> Vec<EngineMeasurementV1> {
    let mut measurements = vec![
        measured("input_tokens", "token", result.observation.input_tokens),
        measured("output_tokens", "token", result.output_tokens),
    ];
    for name in ["compression_saved_tokens", "compression_rate_milli"] {
        if let Some(value) = result.observation.metrics.get(name) {
            let unit = if name == "compression_saved_tokens" {
                "token"
            } else {
                "milliunit"
            };
            measurements.push(measured(name, unit, *value));
        }
    }
    measurements
}

fn measured(name: &str, unit: &str, value: u64) -> EngineMeasurementV1 {
    EngineMeasurementV1 {
        name: name.to_owned(),
        unit: unit.to_owned(),
        classification: EngineValueClassificationV1::Measured,
        value: Some(value),
    }
}

fn source_integrity_failure(invocation: &EngineInvocationV1) -> EngineObservationV1 {
    failed_observation(
        invocation,
        EngineFailureCodeV1::SourceIntegrityMismatch,
        Some(invocation.input_ref.clone()),
    )
}

fn adapter_failure(
    invocation: &EngineInvocationV1,
    failure: &NativeContextInvocationFailure,
) -> EngineObservationV1 {
    let code = match failure {
        NativeContextInvocationFailure::SourceUnavailable(_) => {
            EngineFailureCodeV1::SourceUnavailable
        }
        NativeContextInvocationFailure::ResourceLimit(_) => EngineFailureCodeV1::ResourceLimit,
        NativeContextInvocationFailure::UnsupportedInput => {
            EngineFailureCodeV1::UnsupportedOperation
        }
        NativeContextInvocationFailure::InvalidRequest(_) => EngineFailureCodeV1::Internal,
    };
    let recovery_ref = matches!(code, EngineFailureCodeV1::SourceUnavailable)
        .then(|| invocation.input_ref.clone());
    failed_observation(invocation, code, recovery_ref)
}

fn failed_observation(
    invocation: &EngineInvocationV1,
    code: EngineFailureCodeV1,
    recovery_ref: Option<ProtocolReference>,
) -> EngineObservationV1 {
    EngineObservationV1 {
        schema_version: V1_SCHEMA_VERSION,
        invocation_id: invocation.invocation_id.clone(),
        status: EngineObservationStatusV1::Failed,
        output_ref: None,
        output_digest: None,
        source_lineage: invocation.source_refs.clone(),
        measurements: Vec::new(),
        failure: Some(EngineFailureV1 {
            code,
            retryable_by_host: false,
            recovery_ref,
        }),
        receipt_link: None,
    }
}

fn storage_failure_observation(invocation: &EngineInvocationV1) -> EngineObservationV1 {
    let mut observation = failed_observation(
        invocation,
        EngineFailureCodeV1::Internal,
        Some(invocation.input_ref.clone()),
    );
    if let Some(failure) = observation.failure.as_mut() {
        failure.retryable_by_host = true;
    }
    observation
}

fn persist_output(digest: &str, bytes: &[u8]) -> Result<(), String> {
    persist_content(OUTPUT_DIRECTORY, digest, "txt", bytes)
}

fn persist_receipt(
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
) -> Result<EngineReceiptLinkV1, String> {
    let bytes = canonical_engine_receipt_artifact_bytes(invocation, observation);
    let digest = sha256_digest(&bytes)?;
    persist_content(RECEIPT_DIRECTORY, digest.hex(), "json", &bytes)?;
    Ok(EngineReceiptLinkV1 {
        schema_version: V1_SCHEMA_VERSION,
        receipt_id: ReceiptId::new(format!("engine-receipt-{}", &digest.hex()[..32]))
            .map_err(|error| error.to_string())?,
        receipt_ref: ProtocolReference::new(format!("receipt:sha256:{}", digest.hex()))
            .map_err(|error| error.to_string())?,
        receipt_digest: digest,
        invocation_id: invocation.invocation_id.clone(),
    })
}

fn persist_recovery(
    invocation: &EngineInvocationV1,
    observation: &EngineObservationV1,
    failure_class: &'static str,
) -> Result<ProtocolReference, String> {
    let artifact = EngineRecoveryArtifactV1 {
        schema_version: V1_SCHEMA_VERSION,
        invocation: invocation.clone(),
        observation: observation.clone(),
        failure_class,
    };
    let bytes = canonical::canonical_serialize(&artifact);
    let digest = sha256_digest(&bytes)?;
    persist_content(RECOVERY_DIRECTORY, digest.hex(), "json", &bytes)?;
    ProtocolReference::new(format!("recovery:sha256:{}", digest.hex()))
        .map_err(|error| error.to_string())
}

fn persist_content(
    directory: &str,
    digest: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<(), String> {
    artifact_store::persist_content(directory, digest, extension, bytes).map(|_| ())
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, String> {
    Sha256Digest::new(format!(
        "sha256:{}",
        crate::core::agent_identity::hex_encode(&Sha256::digest(bytes))
    ))
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
