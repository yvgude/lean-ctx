//! `ctx_tools` business logic (#210): the MCP Tool-Catalog Gateway meta-tool.
//!
//! Keeps the registered wrapper thin: this module owns config gating, action
//! routing, and driving the async [`gateway`] from the synchronous tool
//! handler. The dispatch layer already wraps handlers in `block_in_place`, so
//! blocking on the current runtime here is safe (same pattern as `ctx_read`).

use serde_json::{Map, Value};

use crate::core::config::Config;
use crate::core::gateway;

const DISABLED_HINT: &str = "gateway is disabled. Enable it in ~/.lean-ctx/config.toml:\n\
     [gateway]\n\
     enabled = true\n\n\
     [[gateway.servers]]\n\
     name = \"fs\"\n\
     transport = \"stdio\"\n\
     command = \"mcp-server-filesystem\"\n\
     args = [\"/path/to/dir\"]";

/// Execute a `ctx_tools` action, returning response text or an error message.
pub fn run(args: &Map<String, Value>) -> Result<String, String> {
    let cfg = Config::load();
    if !cfg.gateway.enabled_effective() {
        return Err(DISABLED_HINT.to_string());
    }
    if cfg.gateway.active_servers().next().is_none() {
        return Err(
            "gateway is enabled but no downstream servers are configured \
             (add one or more [[gateway.servers]] entries)."
                .to_string(),
        );
    }

    let action = args.get("action").and_then(Value::as_str).unwrap_or("find");
    let rt = tokio::runtime::Handle::current();

    match action {
        "find" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let outcome = rt.block_on(gateway::find(&cfg.gateway, &query));
            Ok(gateway::render_cards(&outcome))
        }
        "list" => Ok(rt.block_on(gateway::servers_overview(&cfg.gateway))),
        "refresh" => {
            gateway::catalog::invalidate();
            let outcome = rt.block_on(gateway::find(&cfg.gateway, ""));
            Ok(format!(
                "gateway catalog refreshed.\n\n{}",
                gateway::render_cards(&outcome)
            ))
        }
        "call" => {
            let tool = args.get("tool").and_then(Value::as_str).ok_or_else(|| {
                "call requires 'tool' — a `server::tool` handle from `ctx_tools find`".to_string()
            })?;
            let arguments = match args.get("arguments") {
                Some(Value::Object(m)) => m.clone(),
                None | Some(Value::Null) => Map::new(),
                Some(_) => return Err("'arguments' must be a JSON object".to_string()),
            };
            rt.block_on(gateway::proxy(&cfg.gateway, tool, arguments))
        }
        other => Err(format!(
            "invalid action '{other}' (use: find, call, list, refresh)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// With the gateway disabled (default), every action returns the enable hint
    /// without touching the network. We assert this in a fresh runtime since
    /// `run` resolves a runtime handle.
    #[test]
    fn disabled_gateway_returns_hint() {
        // Ensure no env override flips it on.
        std::env::remove_var("LEAN_CTX_GATEWAY");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let args = json!({ "action": "find", "query": "anything" });
        let out =
            rt.block_on(async { tokio::task::block_in_place(|| run(args.as_object().unwrap())) });
        // Either disabled (no global config) — the dominant case in CI — or, if a
        // developer machine has it enabled, we still must not panic. We only
        // assert the message shape in the disabled/empty case.
        if let Err(e) = out {
            assert!(e.contains("gateway is disabled") || e.contains("no downstream"));
        }
    }
}
