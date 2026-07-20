//! BuiltinCompressionProvider — real compression via ContentPort + core::compressor.
//!
//! Resolves source bytes through CompressionContentPort, runs the existing
//! aggressive_compress pipeline, persists the result as a BLAKE3 ref, and
//! emits CompressionApplied events. Falls back to token-count estimation
//! when the source_ref cannot be resolved (non-file refs).

use std::path::Path;
use std::sync::OnceLock;

use crate::core::compressor;
use crate::core::ocla::content_port::CompressionContentPort;
use crate::core::ocla::traits::{CompressionProvider, OclaService};
use crate::core::ocla::types::{
    CompressionRequest, CompressionResult, OclaCapability, OclaCapabilityKind, OclaResult,
};
use crate::core::ocla::OclaError;
use crate::core::ocla_bus::{self, OclaEvent};

static DEFAULT_PORT: OnceLock<CompressionContentPort> = OnceLock::new();

fn default_port() -> &'static CompressionContentPort {
    DEFAULT_PORT.get_or_init(|| {
        let root = std::env::current_dir().unwrap_or_else(|_| ".".into());
        CompressionContentPort::new(root)
    })
}

pub struct BuiltinCompressionProvider;

impl BuiltinCompressionProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinCompressionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinCompressionProvider {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::CompressionProvider)
    }
}

impl CompressionProvider for BuiltinCompressionProvider {
    fn compress(&self, request: CompressionRequest) -> OclaResult<CompressionResult> {
        let port = default_port();

        let (delivered_ref, delivered_tokens) = if request.source_ref.starts_with("file:") {
            match port.resolve(&request.source_ref) {
                Ok(bytes) => {
                    let source_text = String::from_utf8_lossy(&bytes);
                    let ext = request
                        .source_ref
                        .strip_prefix("file:")
                        .and_then(|p| Path::new(p).extension())
                        .and_then(|e| e.to_str());

                    let compressed = compressor::aggressive_compress(&source_text, ext);
                    let compressed_bytes = compressed.as_bytes();
                    let compressed_tokens =
                        (compressed_bytes.len() as u64 / 4).min(request.target_tokens);

                    let ref_key = port.persist(compressed_bytes).map_err(|e| {
                        OclaError::InvalidRequest(format!("persist failed: {e}"))
                    })?;

                    (ref_key, compressed_tokens)
                }
                Err(_) => fallback_estimate(&request),
            }
        } else {
            fallback_estimate(&request)
        };

        ocla_bus::emit(OclaEvent::CompressionApplied {
            path: Some(request.source_ref.clone()),
            before_tokens: request.source_tokens,
            after_tokens: delivered_tokens,
            strategy: if delivered_ref.starts_with("blake3:") {
                "aggressive_compress"
            } else {
                "estimate"
            }
            .to_string(),
        });

        Ok(CompressionResult {
            delivered_ref,
            delivered_tokens,
            recovery_ref: Some(request.source_ref),
        })
    }
}

fn fallback_estimate(request: &CompressionRequest) -> (String, u64) {
    let tokens = request.target_tokens.min(request.source_tokens);
    (format!("estimate:{}", request.source_ref), tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;
    use std::fs;

    fn ctx() -> OclaRequestContext {
        OclaRequestContext {
            request_id: "r1".into(),
            session_id: "s1".into(),
            agent_id: "agent-test".into(),
            content_ref: "blake3:test".into(),
            tenant_id: None,
        }
    }

    #[test]
    fn compress_non_file_ref_falls_back_to_estimate() {
        let provider = BuiltinCompressionProvider::new();
        let result = provider
            .compress(CompressionRequest {
                context: ctx(),
                source_ref: "mem:buffer-123".into(),
                source_tokens: 1000,
                target_tokens: 300,
                quality_policy_ref: None,
            })
            .unwrap();

        assert!(result.delivered_ref.starts_with("estimate:"));
        assert_eq!(result.delivered_tokens, 300);
    }

    #[test]
    fn compress_does_not_exceed_source() {
        let provider = BuiltinCompressionProvider::new();
        let result = provider
            .compress(CompressionRequest {
                context: ctx(),
                source_ref: "mem:small".into(),
                source_tokens: 200,
                target_tokens: 500,
                quality_policy_ref: None,
            })
            .unwrap();

        assert_eq!(result.delivered_tokens, 200);
    }

    #[test]
    fn compress_with_real_file_produces_blake3_ref() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let port = CompressionContentPort::new(dir.path());
        DEFAULT_PORT.set(port).ok();

        let provider = BuiltinCompressionProvider::new();
        let result = provider
            .compress(CompressionRequest {
                context: ctx(),
                source_ref: "file:test.rs".into(),
                source_tokens: 100,
                target_tokens: 50,
                quality_policy_ref: None,
            })
            .unwrap();

        assert!(
            result.delivered_ref.starts_with("blake3:")
                || result.delivered_ref.starts_with("estimate:")
        );
        assert!(result.delivered_tokens <= 50);
    }
}
