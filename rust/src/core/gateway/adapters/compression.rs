//! compression adapter (#1101): a downstream compression addon (Headroom / RTK)
//! exposed as a *named* lean-ctx `Compressor` in the extension registry — so it
//! is discoverable via `/v1/capabilities` and selectable by name, exactly like
//! the built-in `identity`/`prose`/`markdown` compressors.
//!
//! Positioning (counter, not lock-in): the addon plugs into lean-ctx as one
//! interchangeable compressor among many, and — because every gateway result
//! flows through the L2 spill path — lean-ctx stays the single retrieval layer
//! (`ctx_expand`). The addon compresses; lean-ctx owns retrieval.
//!
//! Calling a network MCP server from the sync `Compressor::compress` trait is
//! done on a dedicated thread+runtime ([`run_blocking`]), so it is safe from any
//! caller context and never blocks an ambient runtime. Any failure degrades to
//! returning the input unchanged.

use std::sync::{Arc, Once};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::super::client;
use super::super::config::GatewayConfig;
use super::IntegrationKind;
use crate::core::config::Config;
use crate::core::extension_registry::{Compressor, global};

/// A downstream compression addon presented as a lean-ctx compressor.
pub struct GatewayCompressor {
    server: String,
}

impl GatewayCompressor {
    #[must_use]
    pub fn new(server: impl Into<String>) -> Self {
        Self {
            server: server.into(),
        }
    }
}

impl Compressor for GatewayCompressor {
    fn name(&self) -> &str {
        &self.server
    }

    fn compress(&self, input: &str, _budget: Option<usize>) -> String {
        try_compress(&self.server, input).unwrap_or_else(|| input.to_string())
    }
}

/// Register every compression-integration server in `cfg` as a named compressor
/// in the global extension registry. Idempotent; runs at most once per process.
pub fn ensure_registered(cfg: &GatewayConfig) {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for s in cfg.active_servers() {
            if IntegrationKind::parse(&s.integration) == IntegrationKind::Compression
                && let Ok(mut reg) = global().write()
            {
                reg.register_compressor(Arc::new(GatewayCompressor::new(s.name.clone())));
            }
        }
    });
}

/// Route `input` through the server's compression tool via the gateway. Returns
/// `None` (caller keeps the input) when the gateway is off, the server is gone,
/// it exposes no usable tool, or the call fails.
fn try_compress(server: &str, input: &str) -> Option<String> {
    let cfg = Config::load();
    let gw = cfg.gateway;
    if !gw.enabled_effective() {
        return None;
    }
    let srv = gw.active_servers().find(|s| s.name == server)?;
    let transport = srv.resolve().ok()?;
    let timeout = Duration::from_secs(gw.call_timeout_secs.max(1));
    let input = input.to_string();
    let server = server.to_string();

    run_blocking(async move {
        let tools = client::fetch_tools(&transport, timeout).await.ok()?;
        // Prefer an explicit compression tool; fall back to the server's first
        // tool so any single-tool compression server works out of the box.
        let tool = tools
            .iter()
            .find(|t| t.name.to_lowercase().contains("compress"))
            .or_else(|| tools.first())?;
        let arg = first_string_param(tool)?;
        let mut args = Map::new();
        args.insert(arg, json!(input));
        let result = client::proxy_call(&transport, &tool.name, args, timeout)
            .await
            .ok()?;
        Some(crate::core::addons::runtime::scrub_output(
            &server,
            &client::result_to_text(&result),
        ))
    })
}

/// The input-text parameter of a compression tool: a known text-ish name if
/// present, else the first string-typed property, else the first property.
fn first_string_param(tool: &rmcp::model::Tool) -> Option<String> {
    let props = tool.input_schema.get("properties")?.as_object()?;
    const PREFERRED: [&str; 6] = ["text", "input", "content", "code", "message", "data"];
    for name in PREFERRED {
        if props.contains_key(name) {
            return Some(name.to_string());
        }
    }
    for (key, schema) in props {
        if schema.get("type").and_then(Value::as_str) == Some("string") {
            return Some(key.clone());
        }
    }
    props.keys().next().cloned()
}

/// Run a future to completion on a dedicated thread + current-thread runtime, so
/// the sync `compress` works whether or not the caller is inside a runtime.
fn run_blocking<T, F>(fut: F) -> T
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("compressor runtime")
                .block_on(fut)
        })
        .join()
        .expect("compressor thread")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_the_server() {
        let c = GatewayCompressor::new("headroom");
        assert_eq!(c.name(), "headroom");
    }

    #[test]
    fn compress_is_graceful_when_gateway_disabled() {
        crate::test_env::remove_var("LEAN_CTX_GATEWAY");
        // No gateway configured in the test env → input returned unchanged.
        let c = GatewayCompressor::new("nonexistent-server");
        let input = "some text to compress";
        assert_eq!(c.compress(input, None), input);
    }

    #[test]
    fn first_string_param_prefers_known_names() {
        let tool = crate::tool_defs::tool_def(
            "compress_text",
            "desc",
            json!({
                "type": "object",
                "properties": { "level": {"type":"integer"}, "text": {"type":"string"} }
            }),
        );
        assert_eq!(first_string_param(&tool).as_deref(), Some("text"));
    }
}
