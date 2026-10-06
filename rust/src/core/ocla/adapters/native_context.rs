//! Native LeanCTX context optimization adapter.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use lean_ctx_protocol::CapabilityManifestV1;
use sha2::{Digest, Sha256};

use super::read_context_paths;
use crate::core::compressor;
use crate::core::ocla::invocation::{
    CapabilityAdapter, CapabilityInput, CapabilityInvocation, CapabilityObservationV1,
    CapabilityResult, evidence_ref,
};
use crate::core::ocla::{OclaError, OclaResult};
use crate::core::tokens;

/// Repository-relative manifest used by this adapter.
pub const MANIFEST_PATH: &str =
    "docs/contracts/ocla/capability-manifests/leanctx/context-optimization-v1.json";
const MANIFEST_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/ocla/capability-manifests/leanctx/context-optimization-v1.json"
));

pub(crate) fn manifest() -> &'static CapabilityManifestV1 {
    static MANIFEST: OnceLock<CapabilityManifestV1> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let manifest: CapabilityManifestV1 =
            serde_json::from_str(MANIFEST_JSON).expect("native context manifest is valid JSON");
        manifest
            .validate()
            .expect("native context manifest satisfies the protocol");
        manifest
    })
}

/// Local adapter that combines existing file reads and compression primitives.
pub struct NativeContextAdapter {
    root: PathBuf,
    #[cfg(test)]
    materialized_test_control: Option<std::sync::Arc<MaterializedTestControl>>,
}

#[cfg(test)]
pub(crate) struct MaterializedTestControl {
    pub(crate) release: std::sync::Barrier,
    pub(crate) completed: std::sync::Barrier,
}

#[cfg(test)]
impl MaterializedTestControl {
    pub(crate) fn new() -> Self {
        Self {
            release: std::sync::Barrier::new(2),
            completed: std::sync::Barrier::new(2),
        }
    }
}

/// Payload-free result used by the internal Engine proof path.
///
/// Native OCLA consumers keep receiving [`CapabilityResult`]. The Engine needs
/// a SHA-256 identity for the exact derived output, but must not retain or
/// expose that output in a receipt.
#[allow(dead_code)]
pub(crate) struct NativeContextInvocationResult {
    pub result: CapabilityResult,
    pub input_digest: String,
    pub output_digest: String,
    /// Exact derived bytes are kept inside the Engine boundary solely to
    /// persist a local, integrity-addressed output artifact. They are never
    /// serialized, logged, or returned from the OCLA adapter contract.
    pub(crate) output: Vec<u8>,
}

/// Typed internal failure boundary for the native Engine proof path.
///
/// The public OCLA adapter trait continues to expose `OclaError`; the Engine
/// must not infer its stable failure taxonomy by parsing those error strings.
#[derive(Debug)]
pub(crate) enum NativeContextInvocationFailure {
    SourceUnavailable(String),
    ResourceLimit(String),
    UnsupportedInput,
    InvalidRequest(String),
}

impl NativeContextInvocationFailure {
    fn into_ocla_error(self) -> OclaError {
        let message = match self {
            Self::SourceUnavailable(message)
            | Self::ResourceLimit(message)
            | Self::InvalidRequest(message) => message,
            Self::UnsupportedInput => {
                "native context optimization accepts ContextRequest only".to_owned()
            }
        };
        OclaError::InvalidRequest(message)
    }
}

impl NativeContextAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            #[cfg(test)]
            materialized_test_control: None,
        }
    }

    #[must_use]
    pub fn with_root(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            #[cfg(test)]
            materialized_test_control: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_materialized_test_control(
        mut self,
        control: std::sync::Arc<MaterializedTestControl>,
    ) -> Self {
        self.materialized_test_control = Some(control);
        self
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn optimize(&self, content: &str, paths: &[String], mode: &str) -> String {
        if matches!(mode, "full" | "raw" | "passthrough") {
            return content.to_owned();
        }

        let extension = paths
            .first()
            .and_then(|path| Path::new(path).extension())
            .and_then(|extension| extension.to_str());
        compressor::aggressive_compress(content, extension)
    }

    fn invoke_context(
        &self,
        invocation: &CapabilityInvocation,
        paths: &[String],
        mode: &str,
        budget_tokens: Option<u64>,
        start: Instant,
    ) -> Result<NativeContextInvocationResult, NativeContextInvocationFailure> {
        let input = read_context_paths(&self.root, paths).map_err(|error| {
            NativeContextInvocationFailure::SourceUnavailable(error.to_string())
        })?;
        self.invoke_materialized_context(invocation, paths, mode, budget_tokens, start, &input)
    }

    fn invoke_materialized_context(
        &self,
        invocation: &CapabilityInvocation,
        paths: &[String],
        mode: &str,
        budget_tokens: Option<u64>,
        start: Instant,
        input: &str,
    ) -> Result<NativeContextInvocationResult, NativeContextInvocationFailure> {
        let input_tokens = tokens::count_tokens(&input) as u64;
        if let Some(max) = invocation.policy_constraints.max_input_tokens
            && input_tokens > max
        {
            return Err(NativeContextInvocationFailure::ResourceLimit(format!(
                "input exceeds policy token limit ({input_tokens} > {max})"
            )));
        }

        let optimized = self.optimize(&input, paths, mode);
        let output = match budget_tokens {
            Some(budget) => truncate_to_tokens(&optimized, budget),
            None => optimized,
        };
        let output_tokens = tokens::count_tokens(&output) as u64;
        if let Some(max) = invocation.policy_constraints.max_output_tokens
            && output_tokens > max
        {
            return Err(NativeContextInvocationFailure::ResourceLimit(format!(
                "output exceeds policy token limit ({output_tokens} > {max})"
            )));
        }

        let latency_ms = super::super::invocation::check_timeout(start, invocation.timeout_ms)
            .map_err(|error| NativeContextInvocationFailure::ResourceLimit(error.to_string()))?;
        if let Some(max) = invocation.policy_constraints.max_latency_ms
            && latency_ms > max
        {
            return Err(NativeContextInvocationFailure::ResourceLimit(format!(
                "capability latency exceeds policy limit ({latency_ms} > {max})"
            )));
        }
        let output_ref = evidence_ref(&output);
        let mut observation = CapabilityObservationV1::success(
            invocation,
            input_tokens,
            output_tokens,
            latency_ms,
            Some(output_ref.clone()),
        );
        observation.metrics.insert(
            "compression_saved_tokens".into(),
            input_tokens.saturating_sub(output_tokens),
        );
        observation.metrics.insert(
            "compression_rate_milli".into(),
            compression_rate_milli(input_tokens, output_tokens),
        );
        Ok(NativeContextInvocationResult {
            result: CapabilityResult {
                success: true,
                output_tokens,
                latency_ms,
                observation,
                evidence_ref: Some(output_ref),
            },
            input_digest: format!(
                "sha256:{}",
                crate::core::agent_identity::hex_encode(&Sha256::digest(input.as_bytes()))
            ),
            output_digest: format!(
                "sha256:{}",
                crate::core::agent_identity::hex_encode(&Sha256::digest(output.as_bytes()))
            ),
            output: output.into_bytes(),
        })
    }

    /// Execute the native context capability and return its payload-free
    /// SHA-256 output identity for the internal Engine bridge.
    pub(crate) fn invoke_with_output_identity(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<NativeContextInvocationResult, NativeContextInvocationFailure> {
        self.validate_engine_invocation(invocation)?;
        let start = Instant::now();
        match &invocation.input {
            CapabilityInput::ContextRequest {
                paths,
                mode,
                budget_tokens,
            } => self.invoke_context(invocation, paths, mode, *budget_tokens, start),
            CapabilityInput::ShellCommand { .. }
            | CapabilityInput::ModelRequest { .. }
            | CapabilityInput::AgentTask(_) => {
                Err(NativeContextInvocationFailure::UnsupportedInput)
            }
        }
    }

    /// Execute against the exact source snapshot already acquired by a
    /// production caller. This prevents a second file read and binds the
    /// Engine digest to the same bytes the caller will render.
    fn invoke_materialized_with_output_identity(
        &self,
        invocation: &CapabilityInvocation,
        input: &str,
    ) -> Result<NativeContextInvocationResult, NativeContextInvocationFailure> {
        self.validate_engine_invocation(invocation)?;
        let start = Instant::now();
        match &invocation.input {
            CapabilityInput::ContextRequest {
                paths,
                mode,
                budget_tokens,
            } => self.invoke_materialized_context(
                invocation,
                paths,
                mode,
                *budget_tokens,
                start,
                input,
            ),
            CapabilityInput::ShellCommand { .. }
            | CapabilityInput::ModelRequest { .. }
            | CapabilityInput::AgentTask(_) => {
                Err(NativeContextInvocationFailure::UnsupportedInput)
            }
        }
    }

    /// Execute a materialized invocation behind an actual host deadline.
    /// Timed-out workers can finish computation, but cannot persist Engine
    /// artifacts because persistence remains in the receiving Engine layer.
    pub(crate) fn invoke_materialized_bounded(
        &self,
        invocation: CapabilityInvocation,
        input: String,
    ) -> Result<NativeContextInvocationResult, NativeContextInvocationFailure> {
        if invocation.timeout_ms == 0 {
            return Err(NativeContextInvocationFailure::ResourceLimit(
                "native context deadline expired before dispatch".to_owned(),
            ));
        }
        let timeout = std::time::Duration::from_millis(invocation.timeout_ms);
        let adapter = Self::with_root(&self.root);
        #[cfg(test)]
        let adapter = {
            let mut adapter = adapter;
            adapter.materialized_test_control = self.materialized_test_control.clone();
            adapter
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("native-context-engine".to_owned())
            .spawn(move || {
                #[cfg(test)]
                if let Some(control) = adapter.materialized_test_control.as_ref() {
                    control.release.wait();
                }
                let result = adapter.invoke_materialized_with_output_identity(&invocation, &input);
                #[cfg(test)]
                if let Some(control) = adapter.materialized_test_control.as_ref() {
                    control.completed.wait();
                }
                let _ = tx.send(result);
            })
            .map_err(|error| {
                NativeContextInvocationFailure::InvalidRequest(format!(
                    "start native context worker: {error}"
                ))
            })?;
        rx.recv_timeout(timeout).map_err(|error| match error {
            std::sync::mpsc::RecvTimeoutError::Timeout => {
                NativeContextInvocationFailure::ResourceLimit(
                    "native context execution exceeded its deadline".to_owned(),
                )
            }
            std::sync::mpsc::RecvTimeoutError::Disconnected => {
                NativeContextInvocationFailure::InvalidRequest(
                    "native context worker disconnected".to_owned(),
                )
            }
        })?
    }

    fn validate_engine_invocation(
        &self,
        invocation: &CapabilityInvocation,
    ) -> Result<(), NativeContextInvocationFailure> {
        invocation
            .validate()
            .map_err(|error| NativeContextInvocationFailure::InvalidRequest(error.to_string()))?;
        if invocation.capability_id != self.manifest().capability_id.as_str()
            || invocation.capability_version != self.manifest().version
        {
            return Err(NativeContextInvocationFailure::InvalidRequest(
                "invocation capability identity does not match adapter manifest".into(),
            ));
        }
        Ok(())
    }
}

impl Default for NativeContextAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityAdapter for NativeContextAdapter {
    fn manifest(&self) -> &CapabilityManifestV1 {
        manifest()
    }

    fn invoke(&self, invocation: CapabilityInvocation) -> OclaResult<CapabilityResult> {
        self.invoke_with_output_identity(&invocation)
            .map(|result| result.result)
            .map_err(NativeContextInvocationFailure::into_ocla_error)
    }

    fn health_check(&self) -> OclaResult<bool> {
        Ok(self.root.is_dir() && self.manifest().validate().is_ok())
    }
}

fn truncate_to_tokens(content: &str, budget: u64) -> String {
    if budget == 0 {
        return String::new();
    }
    if tokens::count_tokens(content) as u64 <= budget {
        return content.to_owned();
    }
    crate::core::budget::truncate_to_token_budget(
        content,
        usize::try_from(budget).unwrap_or(usize::MAX),
    )
}

fn compression_rate_milli(input_tokens: u64, output_tokens: u64) -> u64 {
    if input_tokens == 0 {
        return 0;
    }
    input_tokens
        .saturating_sub(output_tokens)
        .saturating_mul(1000)
        .checked_div(input_tokens)
        .unwrap_or(0)
        .min(1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::invocation::{CapabilityAdapter, PolicyConstraints};

    #[test]
    fn manifest_is_pinned_to_native_path() {
        assert!(MANIFEST_PATH.ends_with("context-optimization-v1.json"));
        assert_eq!(manifest().provider, "leanctx");
    }

    #[test]
    fn rejects_non_context_inputs() {
        let adapter = NativeContextAdapter::new();
        let invocation = CapabilityInvocation {
            task_id: "task-1".into(),
            capability_id: adapter.manifest().capability_id.as_str().into(),
            capability_version: adapter.manifest().version.clone(),
            input: CapabilityInput::ShellCommand {
                command: "printf test".into(),
                workdir: None,
            },
            policy_constraints: PolicyConstraints::default(),
            timeout_ms: 100,
        };
        assert!(adapter.invoke(invocation).is_err());
    }

    #[test]
    fn internal_output_identity_is_canonical_sha256_without_payload() {
        let root = tempfile::tempdir().expect("temporary adapter root");
        std::fs::write(root.path().join("fixture.md"), "hello context").expect("fixture write");
        let adapter = NativeContextAdapter::with_root(root.path());
        let invocation = CapabilityInvocation {
            task_id: "task-1".into(),
            capability_id: adapter.manifest().capability_id.as_str().into(),
            capability_version: adapter.manifest().version.clone(),
            input: CapabilityInput::ContextRequest {
                paths: vec!["fixture.md".into()],
                mode: "raw".into(),
                budget_tokens: None,
            },
            policy_constraints: PolicyConstraints::default(),
            timeout_ms: 0,
        };

        let result = adapter
            .invoke_with_output_identity(&invocation)
            .expect("native invocation succeeds");
        assert_eq!(
            result.output_digest,
            format!(
                "sha256:{}",
                crate::core::agent_identity::hex_encode(&Sha256::digest(b"hello context"))
            )
        );
        assert!(result.result.evidence_ref.is_some());
    }
}
