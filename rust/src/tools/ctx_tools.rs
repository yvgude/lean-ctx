//! `ctx_tools` business logic (#210): the MCP Tool-Catalog meta-tool.
//!
//! Keeps the registered wrapper thin: this module owns config gating, action
//! routing, and driving the async [`mcp_catalog`] from the synchronous tool
//! handler. Every action runs on the gateway's process-lifetime runtime
//! ([`mcp_catalog::runtime`]), from the MCP dispatch path and the CLI `call`
//! path alike, so the downstream sessions the pool keeps stay alive between
//! calls (#2040).

use serde_json::{Map, Value};

use crate::core::config::Config;
use crate::core::mcp_catalog;

const DISABLED_HINT: &str = "gateway is disabled. Enable it in ~/.lean-ctx/config.toml:\n\
     [gateway]\n\
     enabled = true\n\n\
     [[gateway.servers]]\n\
     name = \"fs\"\n\
     transport = \"stdio\"\n\
     command = \"mcp-server-filesystem\"\n\
     args = [\"/path/to/dir\"]";

use mcp_catalog::runtime::block_on;

/// Execute a `ctx_tools` action, returning response text or an error message.
///
/// `project_root` is the caller's project root (from `ToolContext`); it is
/// forwarded to [`mcp_catalog::proxy`] so output post-processing (#1095) can index
/// downstream results into the project's stores. Empty = no project scope.
pub fn run(args: &Map<String, Value>, project_root: &str) -> Result<String, String> {
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

    // L4: expose any compression-integration addon as a named lean-ctx
    // compressor (once per process). No-op when none are configured.
    mcp_catalog::adapters::compression::ensure_registered(&cfg.gateway);

    let action = args.get("action").and_then(Value::as_str).unwrap_or("find");

    match action {
        "find" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let outcome = block_on(mcp_catalog::find(&cfg.gateway, &query))?;
            Ok(mcp_catalog::render_cards(&outcome))
        }
        "list" => block_on(mcp_catalog::servers_overview(&cfg.gateway)),
        "refresh" => {
            mcp_catalog::catalog::invalidate();
            let outcome = block_on(mcp_catalog::find(&cfg.gateway, ""))?;
            Ok(format!(
                "gateway catalog refreshed.\n\n{}",
                mcp_catalog::render_cards(&outcome)
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
            // K13: no engine rule regulates a downstream server's tools.
            crate::core::hive::metrics_v2::signals::mcp_relay_call(tool);
            block_on(mcp_catalog::proxy(
                &cfg.gateway,
                tool,
                arguments,
                project_root,
            ))?
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
        let _env_lock = crate::core::data_dir::test_env_lock();
        // Ensure no env override flips it on.
        crate::test_env::remove_var("LEAN_CTX_GATEWAY");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let args = json!({ "action": "find", "query": "anything" });
        let out = rt
            .block_on(async { tokio::task::block_in_place(|| run(args.as_object().unwrap(), "")) });
        // Either disabled (no global config) — the dominant case in CI — or, if a
        // developer machine has it enabled, we still must not panic. We only
        // assert the message shape in the disabled/empty case.
        if let Err(e) = out {
            assert!(e.contains("gateway is disabled") || e.contains("no downstream"));
        }
    }

    /// CLI-Pfad-Regression (#ctx_tools): `run` wird OHNE ambienten Tokio-Runtime
    /// aufgerufen (wie `lean-ctx call ctx_tools …`). Früher paniced
    /// `Handle::current()` hier ("no reactor running"); jetzt läuft `run` auf der
    /// prozessweiten Gateway-Runtime. Erwartung: Ok | Err, aber NIEMALS Panik.
    #[test]
    fn run_without_ambient_runtime_does_not_panic() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        crate::test_env::remove_var("LEAN_CTX_GATEWAY");
        let args = json!({ "action": "list" });
        let _ = run(args.as_object().unwrap(), ""); // Ok|Err, niemals Panic
    }
}
