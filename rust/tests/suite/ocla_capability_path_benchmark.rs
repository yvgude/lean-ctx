// SPDX-License-Identifier: Apache-2.0
use std::fs;
use std::time::Instant;

use lean_ctx::core::ocla::builtin::compression_provider::BuiltinCompressionProvider;
use lean_ctx::core::ocla::content_port::CompressionContentPort;
use lean_ctx::core::ocla::registry::OclaRegistry;
use lean_ctx::core::ocla::types::{CompressionRequest, OclaRequestContext};
use lean_ctx::core::tokens::count_tokens;
use lean_ctx_protocol::CapabilityManifestV1;

fn benchmark_content() -> String {
    let removable = std::iter::repeat_n(
        "// Context benchmarking annotation preserves rationale while this redundant narrative is safely removed.\n",
        45,
    )
    .collect::<String>();
    let retained = std::iter::repeat_n("let normalized_context = request.trim().to_owned();\n", 16)
        .collect::<String>();
    format!("{removable}{retained}")
}

fn request_context() -> OclaRequestContext {
    OclaRequestContext {
        request_id: "ocla-capability-path-benchmark".into(),
        session_id: "ocla-capability-path-benchmark".into(),
        agent_id: "ocla-capability-path-benchmark".into(),
        content_ref: "blake3:benchmark".into(),
        tenant_id: None,
        trace_id: "trace-ocla-capability-path-benchmark".into(),
        task_id: None,
        parent_task_id: None,
    }
}

#[test]
fn compression_provider_uses_the_full_registry_capability_path() {
    let content = benchmark_content();
    assert!((5_000..=6_500).contains(&content.len()));

    // Use a tempdir as the port root so the test is self-contained and
    // platform-independent — no dependency on Config::find_project_root()
    // which resolves via git-toplevel (different path format on Windows CI).
    let tmp_dir = tempfile::tempdir().expect("create tempdir for benchmark");
    let path = tmp_dir.path().join(format!(
        "ocla-capability-path-benchmark-{}.txt",
        std::process::id()
    ));
    fs::write(&path, &content).expect("write benchmark content");

    let port =
        CompressionContentPort::new(tmp_dir.path()).expect("port from tempdir should succeed");

    let input_tokens = count_tokens(&content) as u64;
    let provider = BuiltinCompressionProvider::new();
    let started = Instant::now();
    let result = provider
        .compress_with_port(
            CompressionRequest {
                context: request_context(),
                source_ref: format!("file:{}", path.file_name().unwrap().to_string_lossy()),
                source_tokens: input_tokens,
                target_tokens: input_tokens - 1,
                quality_policy_ref: None,
            },
            &port,
        )
        .expect("compression provider should compress benchmark input");
    let latency = started.elapsed();

    assert!(
        result.delivered_tokens < input_tokens,
        "expected compressed output ({} tokens) below input ({input_tokens})",
        result.delivered_tokens
    );
    println!(
        "ocla capability path benchmark: input_tokens={input_tokens} output_tokens={} latency_ms={}",
        result.delivered_tokens,
        latency.as_millis()
    );

    let expected: CapabilityManifestV1 = serde_json::from_str(include_str!(
        "../../../docs/contracts/ocla/capability-manifests/leanctx/context-optimization-v1.json"
    ))
    .expect("pinned compression manifest should parse");
    let direct_manifest = provider
        .manifest()
        .expect("compression manifest should construct");
    direct_manifest
        .validate()
        .expect("compression manifest should be valid");
    assert_eq!(direct_manifest, expected);

    let registry = OclaRegistry::global();
    assert!(
        registry
            .manifests()
            .expect("registry manifests should construct")
            .into_iter()
            .any(|manifest| manifest == expected),
        "registry manifests should include the compression capability contract"
    );
}
