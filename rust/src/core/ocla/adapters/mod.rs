//! Native capability adapters and their versioned registry.

use std::path::{Path, PathBuf};

use super::invocation::CapabilityResult;
use crate::core::io_boundary;
use crate::core::limits;
use crate::core::ocla::OclaError;
use crate::core::ocla::OclaResult;
use crate::core::tokens;

const MAX_CONTEXT_PATHS: usize = 32;

pub mod coverage;
#[cfg(unix)]
pub(crate) mod external_process;
pub mod native_context;
pub mod passthrough;
pub mod registry;

pub use coverage::{
    CAPABILITY_COVERAGE_SCHEMA_VERSION, CapabilityCoverageCaseV1, CapabilityCoverageReportV1,
    CapabilityCoverageResult, CapabilityCoverageScenario,
};
pub use native_context::NativeContextAdapter;
pub use passthrough::PassthroughAdapter;
pub use registry::{AdapterHealth, AdapterKey, AdapterRegistry};

/// Resolve and read a bounded list of context files under an adapter root.
pub(crate) fn read_context_paths(root: &Path, paths: &[String]) -> OclaResult<String> {
    if paths.is_empty() {
        return Err(OclaError::InvalidRequest(
            "context request must contain at least one path".into(),
        ));
    }
    if paths.len() > MAX_CONTEXT_PATHS {
        return Err(OclaError::InvalidRequest(format!(
            "context request exceeds the {MAX_CONTEXT_PATHS} path limit"
        )));
    }

    let root = root
        .canonicalize()
        .map_err(|error| OclaError::InvalidRequest(format!("invalid adapter root: {error}")))?;
    let mut contents = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_usize;
    let max_bytes = limits::max_read_bytes();

    for path in paths {
        if path.trim().is_empty() {
            return Err(OclaError::InvalidRequest(
                "context request path must not be empty".into(),
            ));
        }
        let requested = PathBuf::from(path);
        let candidate = if requested.is_absolute() {
            requested
        } else {
            root.join(requested)
        };
        let canonical = candidate.canonicalize().map_err(|error| {
            OclaError::InvalidRequest(format!("cannot resolve context path {path}: {error}"))
        })?;
        if !canonical.starts_with(&root) {
            return Err(OclaError::InvalidRequest(format!(
                "context path escapes adapter root: {path}"
            )));
        }
        let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
            OclaError::InvalidRequest(format!("cannot inspect context path {path}: {error}"))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(OclaError::InvalidRequest(format!(
                "context path must be a regular non-symlink file: {path}"
            )));
        }
        let declared_bytes = usize::try_from(metadata.len()).map_err(|_| {
            OclaError::InvalidRequest(format!("context path is too large to read safely: {path}"))
        })?;
        let separator_bytes = usize::from(!contents.is_empty());
        if total_bytes
            .checked_add(separator_bytes)
            .and_then(|total| total.checked_add(declared_bytes))
            .is_none_or(|total| total > max_bytes)
        {
            return Err(OclaError::InvalidRequest(format!(
                "context request exceeds the {max_bytes} byte limit"
            )));
        }
        let canonical_string = canonical.to_string_lossy().into_owned();
        let content = io_boundary::read_file_nofollow(&canonical_string).map_err(|error| {
            OclaError::InvalidRequest(format!("cannot read context path {path}: {error}"))
        })?;
        total_bytes = total_bytes
            .checked_add(separator_bytes)
            .and_then(|total| total.checked_add(content.len()))
            .ok_or_else(|| {
                OclaError::InvalidRequest("context request byte count overflowed".into())
            })?;
        if total_bytes > max_bytes {
            return Err(OclaError::InvalidRequest(format!(
                "context request exceeds the {max_bytes} byte limit"
            )));
        }
        contents.push(content);
    }

    Ok(contents.join("\n"))
}

pub(crate) fn invoke_passthrough_text(
    invocation: &super::invocation::CapabilityInvocation,
    text: &str,
    start: std::time::Instant,
) -> OclaResult<CapabilityResult> {
    let input_tokens = tokens::count_tokens(text) as u64;
    if let Some(max) = invocation.policy_constraints.max_input_tokens
        && input_tokens > max
    {
        return Err(OclaError::InvalidRequest(format!(
            "input exceeds policy token limit ({input_tokens} > {max})"
        )));
    }
    let latency_ms = super::invocation::check_timeout(start, invocation.timeout_ms)?;
    if let Some(max) = invocation.policy_constraints.max_latency_ms
        && latency_ms > max
    {
        return Err(OclaError::InvalidRequest(format!(
            "capability latency exceeds policy limit ({latency_ms} > {max})"
        )));
    }
    let output_tokens = input_tokens;
    if let Some(max) = invocation.policy_constraints.max_output_tokens
        && output_tokens > max
    {
        return Err(OclaError::InvalidRequest(format!(
            "output exceeds policy token limit ({output_tokens} > {max})"
        )));
    }
    let output_ref = super::invocation::evidence_ref(text);
    let observation = super::invocation::CapabilityObservationV1::success(
        invocation,
        input_tokens,
        output_tokens,
        latency_ms,
        Some(output_ref.clone()),
    );
    Ok(CapabilityResult {
        success: true,
        output_tokens,
        latency_ms,
        observation,
        evidence_ref: Some(output_ref),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::invocation::{CapabilityAdapter, CapabilityInput, CapabilityInvocation};
    use tempfile::tempdir;

    fn invocation(task_id: &str, capability_id: &str, version: &str) -> CapabilityInvocation {
        CapabilityInvocation {
            task_id: task_id.into(),
            capability_id: capability_id.into(),
            capability_version: version.into(),
            input: CapabilityInput::ContextRequest {
                paths: vec!["fixture.rs".into()],
                mode: "aggressive".into(),
                budget_tokens: Some(100),
            },
            policy_constraints: Default::default(),
            // No deadline: this test compares the two observation envelopes,
            // not latency. With a 1 s deadline the adapter invoked first paid
            // the process's one-time tokenizer and compressor initialization
            // inside its measured window and failed on a loaded CI host
            // ("1235ms > 1000ms", mr-quick job 21704). Deadline enforcement
            // has its own tests (engine_interface for the bounded native
            // path, coverage for external processes).
            timeout_ms: 0,
        }
    }

    #[test]
    fn native_and_passthrough_emit_comparable_observations() {
        let dir = tempdir().expect("fixture directory");
        let fixture = "// repeated documentation\nfn main() {\n    let value = 1;\n}\n";
        std::fs::write(dir.path().join("fixture.rs"), fixture).expect("fixture write");

        let native = NativeContextAdapter::with_root(dir.path());
        let passthrough = PassthroughAdapter::with_root(dir.path());
        let native_result = native
            .invoke(invocation(
                "task-1",
                native.manifest().capability_id.as_str(),
                native.manifest().version.as_str(),
            ))
            .expect("native invocation");
        let passthrough_result = passthrough
            .invoke(invocation(
                "task-1",
                passthrough.manifest().capability_id.as_str(),
                passthrough.manifest().version.as_str(),
            ))
            .expect("passthrough invocation");

        assert!(native_result.success);
        assert!(passthrough_result.success);
        assert_eq!(native_result.observation.task_id, "task-1");
        assert_eq!(
            native_result.observation.input_tokens,
            passthrough_result.observation.input_tokens
        );
        assert!(
            native_result.output_tokens <= passthrough_result.output_tokens,
            "optimization must not emit more tokens for the fixture"
        );
        assert_ne!(
            native_result.evidence_ref, passthrough_result.evidence_ref,
            "the observation envelope must retain strategy-specific evidence"
        );
    }

    #[test]
    fn registry_registers_and_looks_up_by_id_and_version() {
        let registry = AdapterRegistry::new();
        registry
            .register(NativeContextAdapter::new())
            .expect("native registration");
        let adapter = NativeContextAdapter::new();
        let manifest = adapter.manifest().clone();
        let found = registry
            .lookup(manifest.capability_id.as_str(), manifest.version.as_str())
            .expect("registered adapter");
        assert_eq!(found.manifest().capability_id, manifest.capability_id);
        assert_eq!(registry.list_available_adapters().len(), 1);
        assert_eq!(registry.health_check_all().expect("health checks").len(), 1);
    }

    #[test]
    fn context_reads_enforce_path_and_byte_bounds() {
        let _guard = crate::core::data_dir::test_env_lock();
        let dir = tempdir().expect("fixture directory");
        crate::test_env::set_var("LEAN_CTX_MAX_READ_BYTES", "1024");
        std::fs::write(dir.path().join("large.txt"), vec![b'x'; 1025])
            .expect("large fixture write");

        let over_bytes =
            read_context_paths(&dir.path().canonicalize().unwrap(), &["large.txt".into()])
                .expect_err("oversized context must fail");
        assert!(over_bytes.to_string().contains("byte limit"));

        let paths = (0..=MAX_CONTEXT_PATHS)
            .map(|index| format!("fixture-{index}.txt"))
            .collect::<Vec<_>>();
        let over_paths =
            read_context_paths(dir.path(), &paths).expect_err("too many paths must fail");
        assert!(over_paths.to_string().contains("path limit"));
        crate::test_env::remove_var("LEAN_CTX_MAX_READ_BYTES");
    }
}
