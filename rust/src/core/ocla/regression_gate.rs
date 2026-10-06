//! Compression hot-path regression gate (E11, master-plan P4).
//!
//! Compares concrete `BuiltinCompressionProvider` calls with
//! `dyn CompressionProvider` dispatch through `OclaRegistry`.
//! Fails if dynamic dispatch adds more than 200% wall-clock latency
//! (loose enough for shared CI runners; fine-grained tracking in Benchmarks).

#[cfg(test)]
mod tests {
    use std::hint::black_box;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Instant;

    use crate::core::ocla::OclaRegistry;
    use crate::core::ocla::builtin::compression_provider::BuiltinCompressionProvider;
    use crate::core::ocla::content_port::CompressionContentPort;
    use crate::core::ocla::traits::{CompressionProvider, OclaService};
    use crate::core::ocla::types::{
        CompressionRequest, CompressionResult, OclaCapability, OclaCapabilityKind,
        OclaRequestContext, OclaResult,
    };
    use crate::core::tokens;

    const ITERATIONS: usize = 1000;
    /// CI runners share CPUs with other tenants and tests run in parallel,
    /// causing extreme scheduling jitter on timing measurements. 200% still
    /// catches algorithmic regressions (extra allocations, O(n^2) loops) while
    /// tolerating CI noise. Fine-grained perf tracking: dedicated Benchmarks job.
    const MAX_REGRESSION_PCT: f64 = 200.0;
    const SOURCE_REF: &str = "file:rust/src/core/ocla/regression_gate.rs";

    struct ScopedCompressionProvider {
        provider: BuiltinCompressionProvider,
        port: CompressionContentPort,
    }

    impl ScopedCompressionProvider {
        fn new(project_root: &Path) -> Self {
            Self {
                provider: BuiltinCompressionProvider::new(),
                port: CompressionContentPort::new(project_root.to_path_buf())
                    .expect("Engine repository root must be a real directory"),
            }
        }
    }

    impl OclaService for ScopedCompressionProvider {
        fn capability(&self) -> OclaCapability {
            OclaCapability::available(OclaCapabilityKind::CompressionProvider)
        }

        fn manifest(&self) -> OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
            self.provider.manifest()
        }
    }

    impl CompressionProvider for ScopedCompressionProvider {
        fn compress(&self, request: CompressionRequest) -> OclaResult<CompressionResult> {
            self.provider.compress_with_port(request, &self.port)
        }
    }

    fn project_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("Engine crate must be inside its repository")
            .to_path_buf()
    }

    fn make_context() -> OclaRequestContext {
        OclaRequestContext::new(
            "bench-compression".to_string(),
            "bench".to_string(),
            "regression-gate".to_string(),
            SOURCE_REF.to_string(),
            None,
            Some("compression-regression-gate".to_string()),
        )
    }

    fn make_request(source_tokens: u64) -> CompressionRequest {
        CompressionRequest {
            context: make_context(),
            source_ref: SOURCE_REF.to_string(),
            source_tokens,
            target_tokens: source_tokens,
            quality_policy_ref: None,
        }
    }

    fn measure(
        iterations: usize,
        source_tokens: u64,
        mut compress: impl FnMut(CompressionRequest),
    ) -> f64 {
        let requests: Vec<_> = (0..iterations)
            .map(|_| make_request(source_tokens))
            .collect();
        let start = Instant::now();
        for request in requests {
            compress(request);
        }
        start.elapsed().as_nanos() as f64
    }

    #[test]
    fn compression_dyn_dispatch_regression_gate() {
        // Compression resolves process-global policy/runtime paths. Exclude
        // environment edits from both timing samples and admission.
        let _lock = crate::core::data_dir::test_env_lock();
        let project_root = project_root();
        let source_path = SOURCE_REF
            .strip_prefix("file:")
            .expect("source ref must use the file scheme");
        let source = std::fs::read_to_string(project_root.join(source_path))
            .expect("regression-gate source must be readable");
        let source_tokens = tokens::count_tokens(&source) as u64;
        let direct_provider = ScopedCompressionProvider::new(&project_root);
        let mut registry = OclaRegistry::with_builtins();
        registry.compression_provider = Arc::new(ScopedCompressionProvider::new(&project_root));

        for _ in 0..10 {
            black_box(
                direct_provider
                    .compress(make_request(source_tokens))
                    .expect("direct warm-up compression must succeed"),
            );
            black_box(
                registry
                    .compression_provider
                    .compress(make_request(source_tokens))
                    .expect("dynamic warm-up compression must succeed"),
            );
        }

        let direct_ns = measure(ITERATIONS, source_tokens, |request| {
            black_box(
                direct_provider
                    .compress(request)
                    .expect("direct compression must succeed"),
            );
        });

        let dyn_ns = measure(ITERATIONS, source_tokens, |request| {
            black_box(
                registry
                    .compression_provider
                    .compress(request)
                    .expect("dynamic compression must succeed"),
            );
        });

        let regression_pct = ((dyn_ns - direct_ns) / direct_ns) * 100.0;
        eprintln!(
            "Compression regression gate: direct={:.0}ns, dyn={:.0}ns, delta={:.2}%",
            direct_ns / ITERATIONS as f64,
            dyn_ns / ITERATIONS as f64,
            regression_pct
        );

        assert!(
            regression_pct < MAX_REGRESSION_PCT,
            "dyn CompressionProvider adds {regression_pct:.2}% latency \
             (max {MAX_REGRESSION_PCT}%)"
        );
    }
}
