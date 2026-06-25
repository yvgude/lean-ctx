//! Aggregated downstream tool catalog (#210) with a TTL cache.
//!
//! Connects to every enabled downstream server, namespaces each tool as
//! `server::tool`, and caches the union in-process for `cache_ttl_secs`. Fetch
//! errors are *surfaced* (collected into [`Catalog::errors`]) rather than
//! silently dropped, so a misconfigured server is visible to the agent.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::client;
use super::config::GatewayConfig;

/// One downstream tool, namespaced by its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub server: String,
    pub tool: String,
    /// `server::tool` — the stable, collision-free handle used by `ctx_tools`.
    pub namespaced: String,
    pub description: String,
    /// Compact `param, required*` summary for `ChoiceCards`.
    pub params: String,
}

/// The aggregated catalog plus any per-server fetch errors.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
    pub errors: Vec<String>,
}

impl Catalog {
    /// Look up an entry by its `server::tool` handle.
    #[must_use]
    pub fn find(&self, namespaced: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| e.namespaced == namespaced)
    }

    /// Distinct server names present in the catalog, sorted.
    #[must_use]
    pub fn server_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.entries.iter().map(|e| e.server.clone()).collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Split a `server::tool` handle back into its parts.
#[must_use]
pub fn split_namespaced(handle: &str) -> Option<(&str, &str)> {
    handle
        .split_once("::")
        .filter(|(s, t)| !s.is_empty() && !t.is_empty())
}

static CACHE: Mutex<Option<(Instant, Catalog)>> = Mutex::new(None);

/// Drop the cached catalog so the next [`get`] rebuilds it.
pub fn invalidate() {
    if let Ok(mut g) = CACHE.lock() {
        *g = None;
    }
}

/// Return the catalog, rebuilding it if the cache is empty or older than
/// `cache_ttl_secs`.
pub async fn get(cfg: &GatewayConfig) -> Catalog {
    let ttl = Duration::from_secs(cfg.cache_ttl_secs);
    if let Ok(guard) = CACHE.lock()
        && let Some((at, cat)) = guard.as_ref()
        && at.elapsed() < ttl
    {
        return cat.clone();
    }
    let fresh = build(cfg).await;
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((Instant::now(), fresh.clone()));
    }
    fresh
}

/// Build the catalog from scratch (no cache). Connects to each enabled server.
pub async fn build(cfg: &GatewayConfig) -> Catalog {
    let timeout = Duration::from_secs(cfg.call_timeout_secs.max(1));
    let mut entries: Vec<CatalogEntry> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for server in cfg.active_servers() {
        // Kill-switch (P2): a revoked server is dropped from the catalog so its
        // tools cannot be discovered or called; the reason is surfaced.
        if let Some(reason) = crate::core::addons::revocation::blocked_reason(&server.name) {
            errors.push(format!("{}: revoked — {reason}", server.name));
            continue;
        }
        let resolved = match server.resolve() {
            Ok(r) => r,
            Err(e) => {
                errors.push(e);
                continue;
            }
        };
        match client::fetch_tools(&resolved, timeout).await {
            Ok(tools) => {
                for t in tools {
                    let tool = t.name.to_string();
                    entries.push(CatalogEntry {
                        namespaced: format!("{}::{}", server.name, tool),
                        server: server.name.clone(),
                        description: t.description.as_deref().unwrap_or("").trim().to_string(),
                        params: summarize_schema(t.input_schema.as_ref()),
                        tool,
                    });
                }
            }
            Err(e) => errors.push(format!("{}: {e}", server.name)),
        }
    }

    // Stable order + collision-free dedup by handle.
    entries.sort_by(|a, b| a.namespaced.cmp(&b.namespaced));
    entries.dedup_by(|a, b| a.namespaced == b.namespaced);
    errors.sort();
    errors.dedup();

    Catalog { entries, errors }
}

/// Render a JSON-Schema `properties`/`required` object into a compact
/// `name, required*` parameter list (required params get a trailing `*`).
fn summarize_schema(schema: &Map<String, Value>) -> String {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return String::new();
    };
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut names: Vec<String> = props
        .keys()
        .map(|k| {
            if required.contains(k.as_str()) {
                format!("{k}*")
            } else {
                k.clone()
            }
        })
        .collect();
    names.sort();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_namespaced_parses_handle() {
        assert_eq!(split_namespaced("fs::read_file"), Some(("fs", "read_file")));
        assert_eq!(split_namespaced("noseparator"), None);
        assert_eq!(split_namespaced("::x"), None);
        assert_eq!(split_namespaced("x::"), None);
    }

    #[test]
    fn summarize_schema_marks_required() {
        let schema = json!({
            "type": "object",
            "properties": { "path": {"type":"string"}, "depth": {"type":"integer"} },
            "required": ["path"]
        });
        let s = summarize_schema(schema.as_object().unwrap());
        assert_eq!(s, "depth, path*");
    }

    #[test]
    fn summarize_schema_empty_when_no_props() {
        let schema = json!({ "type": "object" });
        assert_eq!(summarize_schema(schema.as_object().unwrap()), "");
    }

    #[tokio::test]
    async fn revoked_server_is_dropped_from_catalog() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let mut list = crate::core::addons::revocation::RevocationList::load();
        list.revoke("blocked", "kill-switch test", None);
        list.save().expect("save");

        let cfg = GatewayConfig {
            enabled: true,
            servers: vec![crate::core::gateway::GatewayServer {
                name: "blocked".into(),
                command: "true".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let cat = build(&cfg).await;
        // Revoked server never spawned; it contributes no tools, only an error.
        assert!(cat.entries.is_empty());
        assert!(
            cat.errors.iter().any(|e| e.contains("revoked")),
            "errors: {:?}",
            cat.errors
        );
    }

    #[test]
    fn catalog_find_and_servers() {
        let cat = Catalog {
            entries: vec![
                CatalogEntry {
                    server: "fs".into(),
                    tool: "read".into(),
                    namespaced: "fs::read".into(),
                    description: "Read a file".into(),
                    params: "path*".into(),
                },
                CatalogEntry {
                    server: "git".into(),
                    tool: "log".into(),
                    namespaced: "git::log".into(),
                    description: "Show log".into(),
                    params: String::new(),
                },
            ],
            errors: vec![],
        };
        assert_eq!(cat.find("fs::read").unwrap().tool, "read");
        assert!(cat.find("missing::x").is_none());
        assert_eq!(cat.server_names(), vec!["fs", "git"]);
    }
}
