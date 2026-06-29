//! `ctx_tools` business logic (#210): the MCP Tool-Catalog Gateway meta-tool.
//!
//! Keeps the registered wrapper thin: this module owns config gating, action
//! routing, and driving the async [`gateway`] from the synchronous tool
//! handler. The MCP dispatch layer wraps handlers in `block_in_place`, so an ambient
//! runtime handle is available there. The CLI `call` path has no ambient
//! runtime, so `run` falls back to building its own current_thread runtime
//! (see `Rt`) instead of panicking on `Handle::current()`.

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

/// Runtime adapter so the `rt.block_on(...)` call sites stay identical whether
/// we run inside the MCP server's runtime (ambient `Handle`) or from the CLI
/// `call` path, which has no ambient runtime and needs its own.
enum Rt {
    Handle(tokio::runtime::Handle),
    Owned(tokio::runtime::Runtime),
}

impl Rt {
    fn block_on<F: std::future::Future>(&self, fut: F) -> F::Output {
        match self {
            Rt::Handle(h) => h.block_on(fut),
            Rt::Owned(r) => r.block_on(fut),
        }
    }
}

/// Execute a `ctx_tools` action, returning response text or an error message.
///
/// `project_root` is the caller's project root (from [`ToolContext`]); it is
/// forwarded to [`gateway::proxy`] so output post-processing (#1095) can index
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
    gateway::adapters::compression::ensure_registered(&cfg.gateway);

    let action = args.get("action").and_then(Value::as_str).unwrap_or("find");
    let rt = match tokio::runtime::Handle::try_current() {
        Ok(h) => Rt::Handle(h), // MCP path: dispatch already did block_in_place
        Err(_) => Rt::Owned(
            // CLI path: no ambient runtime → make a one-shot current_thread one
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("failed to start runtime for gateway: {e}"))?,
        ),
    };

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
            rt.block_on(gateway::proxy(&cfg.gateway, tool, arguments, project_root))
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
    /// `Handle::current()` hier ("no reactor running"); jetzt baut `run` selbst
    /// einen current_thread-Runtime. Erwartung: Ok | Err, aber NIEMALS Panik.
    #[test]
    fn run_without_ambient_runtime_does_not_panic() {
        crate::test_env::remove_var("LEAN_CTX_GATEWAY");
        let args = json!({ "action": "list" });
        let _ = run(args.as_object().unwrap(), ""); // Ok|Err, niemals Panic
    }
}
