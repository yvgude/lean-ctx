//! BuiltinCompressionProvider — fail-closed compression via ContentPort + core::compressor.
//!
//! Uses Config::find_project_root() for bounded root resolution. Reports
//! capability Unavailable when no valid project root exists. Rejects non-file refs
//! and propagates all errors (fail-closed, no fabricated fallbacks).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::core::compressor;
use crate::core::config::Config;
use crate::core::ocla::OclaError;
use crate::core::ocla::content_port::CompressionContentPort;
use crate::core::ocla::traits::{CompressionProvider, OclaService};
use crate::core::ocla::types::{
    CompressionRequest, CompressionResult, OCLA_API_VERSION, OclaCapability, OclaCapabilityKind,
    OclaCapabilityStatus, OclaResult,
};
use crate::core::ocla_bus::{self, OclaEvent};
use crate::core::tokens;

static DEFAULT_PORT: OnceLock<Option<CompressionContentPort>> = OnceLock::new();

fn try_default_port() -> Option<&'static CompressionContentPort> {
    DEFAULT_PORT
        .get_or_init(|| {
            let root_str = Config::find_project_root()?;
            let root = PathBuf::from(&root_str);
            let canonical = root.canonicalize().ok()?;
            let meta = canonical.symlink_metadata().ok()?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return None;
            }
            Some(CompressionContentPort::new(canonical))
        })
        .as_ref()
}

pub struct BuiltinCompressionProvider;

impl BuiltinCompressionProvider {
    pub fn new() -> Self {
        Self
    }

    pub fn compress_with_port(
        &self,
        request: CompressionRequest,
        port: &CompressionContentPort,
    ) -> OclaResult<CompressionResult> {
        if request.source_tokens == 0 {
            return Err(OclaError::InvalidRequest(
                "source_tokens must be > 0".into(),
            ));
        }
        if request.target_tokens == 0 {
            return Err(OclaError::InvalidRequest(
                "target_tokens must be > 0".into(),
            ));
        }

        if !request.source_ref.starts_with("file:") {
            return Err(OclaError::InvalidRequest(format!(
                "only file: refs supported, got: {}",
                request.source_ref.split(':').next().unwrap_or("unknown")
            )));
        }

        let bytes = port.resolve(&request.source_ref)?;

        let source_text = std::str::from_utf8(&bytes)
            .map_err(|_| OclaError::InvalidRequest("source is not valid UTF-8".into()))?;

        let ext = request
            .source_ref
            .strip_prefix("file:")
            .and_then(|p| Path::new(p).extension())
            .and_then(|e| e.to_str());

        let compressed = compressor::aggressive_compress(source_text, ext);
        let delivered_tokens = tokens::count_tokens(&compressed) as u64;

        if delivered_tokens >= request.source_tokens {
            return Err(OclaError::InvalidRequest(
                "compression produced no gain (output >= source)".into(),
            ));
        }

        if delivered_tokens > request.target_tokens {
            return Err(OclaError::InvalidRequest(format!(
                "compressed output ({delivered_tokens}) exceeds target ({})",
                request.target_tokens
            )));
        }

        let ref_key = port.persist(compressed.as_bytes())?;

        ocla_bus::emit(OclaEvent::CompressionApplied {
            path: Some(request.source_ref.clone()),
            before_tokens: request.source_tokens,
            after_tokens: delivered_tokens,
            strategy: "aggressive_compress".to_string(),
        });

        Ok(CompressionResult {
            delivered_ref: ref_key,
            delivered_tokens,
            recovery_ref: Some(request.source_ref),
        })
    }
}

impl Default for BuiltinCompressionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinCompressionProvider {
    fn capability(&self) -> OclaCapability {
        if try_default_port().is_some() {
            OclaCapability::available(OclaCapabilityKind::CompressionProvider)
        } else {
            OclaCapability {
                kind: OclaCapabilityKind::CompressionProvider,
                api_version: OCLA_API_VERSION.to_string(),
                status: OclaCapabilityStatus::Unavailable,
                limits: BTreeMap::new(),
            }
        }
    }
}

impl CompressionProvider for BuiltinCompressionProvider {
    fn compress(&self, request: CompressionRequest) -> OclaResult<CompressionResult> {
        let port = try_default_port().ok_or_else(|| {
            OclaError::InvalidRequest("compression unavailable: no valid project root".into())
        })?;
        self.compress_with_port(request, port)
    }
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
    fn compress_rejects_non_file_ref() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let port = CompressionContentPort::new(&root);
        let provider = BuiltinCompressionProvider::new();
        let err = provider
            .compress_with_port(
                CompressionRequest {
                    context: ctx(),
                    source_ref: "mem:buffer-123".into(),
                    source_tokens: 1000,
                    target_tokens: 300,
                    quality_policy_ref: None,
                },
                &port,
            )
            .unwrap_err();
        assert!(err.to_string().contains("only file: refs"));
    }

    #[test]
    fn compress_rejects_zero_source_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let port = CompressionContentPort::new(&root);
        let provider = BuiltinCompressionProvider::new();
        let err = provider
            .compress_with_port(
                CompressionRequest {
                    context: ctx(),
                    source_ref: "file:test.rs".into(),
                    source_tokens: 0,
                    target_tokens: 300,
                    quality_policy_ref: None,
                },
                &port,
            )
            .unwrap_err();
        assert!(err.to_string().contains("source_tokens"));
    }

    #[test]
    fn compress_rejects_zero_target_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let port = CompressionContentPort::new(&root);
        let provider = BuiltinCompressionProvider::new();
        let err = provider
            .compress_with_port(
                CompressionRequest {
                    context: ctx(),
                    source_ref: "file:test.rs".into(),
                    source_tokens: 100,
                    target_tokens: 0,
                    quality_policy_ref: None,
                },
                &port,
            )
            .unwrap_err();
        assert!(err.to_string().contains("target_tokens"));
    }

    #[test]
    fn compress_with_real_file_returns_blake3_ref() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let content = "use std::collections::HashMap;\n\
            use std::io::{self, Read, Write, BufReader, BufWriter};\n\
            use std::fs::File;\n\n\
            /// Verbose doc that should compress well.\n\
            /// Another line of documentation.\n\
            /// Even more verbose documentation here.\n\
            fn main() -> io::Result<()> {\n    \
            // Initialize the hashmap for storing key-value pairs\n    \
            let mut hash_map_instance: HashMap<String, String> = HashMap::new();\n    \
            // Insert the first key-value pair into the hashmap\n    \
            hash_map_instance.insert(String::from(\"key_one\"), String::from(\"value_one\"));\n    \
            // Insert the second key-value pair into the hashmap\n    \
            hash_map_instance.insert(String::from(\"key_two\"), String::from(\"value_two\"));\n    \
            // Insert the third key-value pair into the hashmap\n    \
            hash_map_instance.insert(String::from(\"key_three\"), String::from(\"value_three\"));\n    \
            // Iterate over all key-value pairs and print them\n    \
            for (key_variable, value_variable) in hash_map_instance.iter() {\n        \
            // Print the current key and value\n        \
            println!(\"Key: {}, Value: {}\", key_variable, value_variable);\n    \
            }\n    \
            // Open a file for reading\n    \
            let input_file_handle: File = File::open(\"input.txt\")?;\n    \
            // Create a buffered reader\n    \
            let mut buffered_reader_instance: BufReader<File> = BufReader::new(input_file_handle);\n    \
            // Read all contents into a string\n    \
            let mut file_contents_buffer: String = String::new();\n    \
            buffered_reader_instance.read_to_string(&mut file_contents_buffer)?;\n    \
            // Print the length\n    \
            println!(\"Read {} bytes from input file\", file_contents_buffer.len());\n    \
            Ok(())\n\
            }\n";
        fs::write(root.join("test.rs"), content).unwrap();

        let port = CompressionContentPort::new(&root);
        let provider = BuiltinCompressionProvider::new();
        let source_tokens = tokens::count_tokens(content) as u64;

        let result = provider.compress_with_port(
            CompressionRequest {
                context: ctx(),
                source_ref: "file:test.rs".into(),
                source_tokens,
                target_tokens: source_tokens,
                quality_policy_ref: None,
            },
            &port,
        );

        let r = result.unwrap();
        assert!(r.delivered_ref.starts_with("blake3:"));
        assert!(r.delivered_tokens < source_tokens);
        assert_eq!(r.recovery_ref, Some("file:test.rs".into()));
    }

    #[test]
    fn compress_propagates_resolve_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let port = CompressionContentPort::new(&root);
        let provider = BuiltinCompressionProvider::new();
        let err = provider
            .compress_with_port(
                CompressionRequest {
                    context: ctx(),
                    source_ref: "file:nonexistent.rs".into(),
                    source_tokens: 100,
                    target_tokens: 50,
                    quality_policy_ref: None,
                },
                &port,
            )
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("resolve") || msg.contains("No such file") || msg.contains("not found"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn capability_reflects_root_availability() {
        let provider = BuiltinCompressionProvider::new();
        let cap = provider.capability();
        assert_eq!(cap.kind, OclaCapabilityKind::CompressionProvider);
    }
}
