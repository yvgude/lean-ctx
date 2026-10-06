//! Passthrough/control adapter for native A/B comparisons.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use lean_ctx_protocol::CapabilityManifestV1;

use super::{invoke_passthrough_text, read_context_paths};
use crate::core::ocla::invocation::{
    CapabilityAdapter, CapabilityInput, CapabilityInvocation, CapabilityResult,
};
use crate::core::ocla::{OclaError, OclaResult};

/// Repository-relative manifest used by this adapter.
pub const MANIFEST_PATH: &str =
    "docs/contracts/ocla/capability-manifests/leanctx/passthrough-v1.json";
const MANIFEST_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/ocla/capability-manifests/leanctx/passthrough-v1.json"
));

fn manifest() -> &'static CapabilityManifestV1 {
    static MANIFEST: OnceLock<CapabilityManifestV1> = OnceLock::new();
    MANIFEST.get_or_init(|| {
        let manifest: CapabilityManifestV1 =
            serde_json::from_str(MANIFEST_JSON).expect("passthrough manifest is valid JSON");
        manifest
            .validate()
            .expect("passthrough manifest satisfies the protocol");
        manifest
    })
}

/// Baseline adapter that returns the materialized input unchanged.
pub struct PassthroughAdapter {
    root: PathBuf,
}

impl PassthroughAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    #[must_use]
    pub fn with_root(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Default for PassthroughAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityAdapter for PassthroughAdapter {
    fn manifest(&self) -> &CapabilityManifestV1 {
        manifest()
    }

    fn invoke(&self, invocation: CapabilityInvocation) -> OclaResult<CapabilityResult> {
        invocation.validate()?;
        if invocation.capability_id != self.manifest().capability_id.as_str()
            || invocation.capability_version != self.manifest().version
        {
            return Err(OclaError::InvalidRequest(
                "invocation capability identity does not match adapter manifest".into(),
            ));
        }
        let start = Instant::now();
        match invocation.input.clone() {
            CapabilityInput::ContextRequest { paths, .. } => {
                let content = read_context_paths(&self.root, &paths)?;
                invoke_passthrough_text(&invocation, &content, start)
            }
            CapabilityInput::ShellCommand { command, .. } => {
                invoke_passthrough_text(&invocation, &command, start)
            }
            CapabilityInput::ModelRequest { prompt, .. } => {
                invoke_passthrough_text(&invocation, &prompt, start)
            }
            CapabilityInput::AgentTask(_) => Err(OclaError::InvalidRequest(
                "passthrough cannot execute agent tasks".into(),
            )),
        }
    }

    fn health_check(&self) -> OclaResult<bool> {
        Ok(self.root.is_dir() && self.manifest().validate().is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::invocation::{CapabilityAdapter, PolicyConstraints};

    #[test]
    fn preserves_shell_command_content_reference() {
        let adapter = PassthroughAdapter::new();
        let invocation = CapabilityInvocation {
            task_id: "task-1".into(),
            capability_id: adapter.manifest().capability_id.as_str().into(),
            capability_version: adapter.manifest().version.clone(),
            input: CapabilityInput::ShellCommand {
                command: "printf unchanged".into(),
                workdir: None,
            },
            policy_constraints: PolicyConstraints::default(),
            // This test checks content identity, not machine-dependent tokenizer startup time.
            timeout_ms: 0,
        };
        let result = adapter.invoke(invocation).expect("passthrough invocation");
        assert!(result.success);
        assert_eq!(result.observation.input_tokens, result.output_tokens);
        assert!(result.evidence_ref.is_some());
    }
}
