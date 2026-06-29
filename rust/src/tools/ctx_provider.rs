use crate::core::consolidation;
use crate::core::providers::cache as provider_cache;
use crate::core::providers::config::GitLabConfig;
use crate::core::providers::provider_trait::ProviderParams;
use crate::core::providers::registry::global_registry;
use crate::core::providers::{ProviderResult, gitlab};
use crate::server::tool_trait::ToolContext;

pub fn handle(args: &serde_json::Map<String, serde_json::Value>, ctx: &ToolContext) -> String {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        // -- Discovery & Management --
        "discover" | "list" => handle_discover(ctx),
        "status" => handle_status(ctx),
        "refresh" => handle_refresh(args, ctx),
        "configure" => handle_configure(args, ctx),

        // -- Registry-based routing (provider_id + resource) --
        "query" => handle_registry_query(args, ctx),

        // -- MCP Bridge convenience actions --
        "mcp_resources" => handle_mcp_resources(args, ctx),

        // -- Legacy GitLab actions (backward-compatible) --
        "gitlab_issues" => handle_gitlab_issues(args),
        "gitlab_issue" => handle_gitlab_issue(args),
        "gitlab_mrs" => handle_gitlab_mrs(args),
        "gitlab_pipelines" => handle_gitlab_pipelines(args),

        _ => {
            let available = "discover, list, status, refresh, configure, query, mcp_resources, \
                 gitlab_issues, gitlab_issue, gitlab_mrs, gitlab_pipelines";
            format!("Unknown action: {action}. Available: {available}")
        }
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

fn handle_discover(ctx: &ToolContext) -> String {
    crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(
        &ctx.project_root,
    )));
    let infos = global_registry().discover();
    if infos.is_empty() {
        return "No providers registered. Set GITHUB_TOKEN or GITLAB_TOKEN.".to_string();
    }

    let mut out = format!("Registered providers ({}):\n", infos.len());
    for info in &infos {
        let status = if info.available {
            "ready"
        } else {
            "unavailable"
        };
        out.push_str(&format!(
            "  {} ({}) [{}] actions: {}\n",
            info.id,
            info.display_name,
            status,
            info.actions.join(", "),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Status — provider health + cache metrics
// ---------------------------------------------------------------------------

fn handle_status(ctx: &ToolContext) -> String {
    crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(
        &ctx.project_root,
    )));

    let infos = global_registry().discover();
    let metrics = provider_cache::cache_metrics();

    let mut out = String::new();

    // Provider health
    out.push_str(&format!("Provider Status ({} registered):\n", infos.len()));
    for info in &infos {
        let status = if info.available { "✓" } else { "✗" };
        let auth = if info.requires_auth { " (auth)" } else { "" };
        out.push_str(&format!(
            "  {status} {} — {} [ttl:{}s]{auth}\n",
            info.id, info.display_name, info.cache_ttl_secs,
        ));
    }

    // Cache metrics
    out.push_str(&format!(
        "\nCache: {} entries, {:.0}% hit rate ({} hits / {} misses)\n",
        metrics.total_entries,
        metrics.total_hit_rate() * 100.0,
        metrics.total_hits,
        metrics.total_misses,
    ));

    if !metrics.provider_stats.is_empty() {
        out.push_str("Per-provider:\n");
        for ps in &metrics.provider_stats {
            let last = ps
                .last_fetch
                .and_then(|t| t.elapsed().ok())
                .map_or_else(|| "never".into(), |d| format!("{}s ago", d.as_secs()));
            out.push_str(&format!(
                "  {} — {} cached, {:.0}% hit rate, last fetch: {}\n",
                ps.provider_id,
                ps.entry_count,
                ps.hit_rate() * 100.0,
                last,
            ));
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Refresh — invalidate cache + re-fetch + re-consolidate
// ---------------------------------------------------------------------------

fn handle_refresh(args: &serde_json::Map<String, serde_json::Value>, ctx: &ToolContext) -> String {
    crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(
        &ctx.project_root,
    )));

    let provider_id = args.get("provider").and_then(|v| v.as_str());
    let resource = args.get("resource").and_then(|v| v.as_str());

    // Invalidate cache
    let invalidated = if let Some(pid) = provider_id {
        let count = provider_cache::invalidate_provider(pid);
        format!("Invalidated {count} cached entries for '{pid}'")
    } else {
        let count = provider_cache::invalidate_all();
        format!("Invalidated {count} cached entries (all providers)")
    };

    let mut out = format!("{invalidated}\n");

    // Re-fetch if provider + resource specified
    if let (Some(pid), Some(res)) = (provider_id, resource) {
        let params = ProviderParams {
            state: args.get("state").and_then(|v| v.as_str()).map(String::from),
            limit: args
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as usize),
            ..Default::default()
        };

        match global_registry().execute_as_chunks(pid, res, &params) {
            Ok(chunks) => {
                consolidate_to_session(&chunks, ctx);
                out.push_str(&format!(
                    "Re-fetched {pid}/{res}: {} items, consolidated to BM25+Graph+Knowledge\n",
                    chunks.len()
                ));
            }
            Err(e) => out.push_str(&format!("Re-fetch failed: {e}\n")),
        }
    } else if let Some(pid) = provider_id {
        // Refresh all actions for a single provider
        let registry = global_registry();
        match registry.get(pid) {
            Some(provider) => {
                let mut total = 0;
                for action in provider.supported_actions() {
                    let params = ProviderParams {
                        limit: Some(20),
                        ..Default::default()
                    };
                    match registry.execute_as_chunks(pid, action, &params) {
                        Ok(chunks) => {
                            consolidate_to_session(&chunks, ctx);
                            total += chunks.len();
                        }
                        Err(e) => {
                            tracing::debug!("[ctx_provider] refresh {pid}/{action} failed: {e}");
                        }
                    }
                }
                out.push_str(&format!(
                    "Re-fetched all actions for '{pid}': {total} items consolidated\n"
                ));
            }
            _ => {
                out.push_str(&format!("Provider '{pid}' not found\n"));
            }
        }
    } else {
        out.push_str("Specify provider= to also re-fetch data after cache invalidation\n");
    }

    out
}

// ---------------------------------------------------------------------------
// Configure — show config paths + available config providers
// ---------------------------------------------------------------------------

fn handle_configure(
    args: &serde_json::Map<String, serde_json::Value>,
    ctx: &ToolContext,
) -> String {
    let sub = args
        .get("resource")
        .and_then(|v| v.as_str())
        .unwrap_or("show");

    match sub {
        "paths" => {
            let mut out = String::from("Provider config locations (checked in order):\n");
            out.push_str("  Single-file (providers.toml):\n");
            if let Ok(config_dir) = crate::core::paths::config_dir() {
                let p = config_dir.join("providers.toml");
                let exists = if p.exists() { " ✓" } else { "" };
                out.push_str(&format!("    {}{exists}\n", p.display()));
            }
            if let Some(home) = dirs::home_dir() {
                let p = home.join(".lean-ctx").join("providers.toml");
                let exists = if p.exists() { " ✓" } else { "" };
                out.push_str(&format!("    {}{exists}\n", p.display()));
            }
            let p = std::path::Path::new(&ctx.project_root)
                .join(".lean-ctx")
                .join("providers.toml");
            let exists = if p.exists() { " ✓" } else { "" };
            out.push_str(&format!("    {}{exists}\n", p.display()));

            out.push_str("  Per-file (one provider per file):\n");
            if let Ok(config_dir) = crate::core::paths::config_dir() {
                let p = config_dir.join("providers");
                let exists = if p.exists() { " ✓" } else { "" };
                out.push_str(&format!("    {}/{exists}\n", p.display()));
            }
            let p = std::path::Path::new(&ctx.project_root)
                .join(".lean-ctx")
                .join("providers");
            let exists = if p.exists() { " ✓" } else { "" };
            out.push_str(&format!("    {}/{exists}\n", p.display()));

            out.push_str("\nEnvironment variables:\n");
            for (var, label) in [
                ("GITHUB_TOKEN", "GitHub"),
                ("GITLAB_TOKEN", "GitLab"),
                ("JIRA_URL", "Jira"),
                ("DATABASE_URL", "PostgreSQL"),
            ] {
                let set = if std::env::var(var).is_ok() {
                    "✓ set"
                } else {
                    "✗ not set"
                };
                out.push_str(&format!("  {var} ({label}): {set}\n"));
            }
            out
        }
        "template" => String::from(
            r#"# providers.toml — drop in ~/.config/lean-ctx/ or .lean-ctx/
# Each [[providers]] entry registers a custom REST API as a context source.

[[providers]]
id = "linear"
name = "Linear"
base_url = "https://api.linear.app"
cache_ttl_secs = 120

[providers.auth]
type = "bearer"
token_env = "LINEAR_API_KEY"

[providers.resources.issues]
method = "POST"
path = "/graphql"

[providers.resources.issues.response]
root = "data.issues.nodes"

[providers.resources.issues.response.mapping]
id = "id"
title = "title"
body = "description"
state = "state.name"
labels = "labels.nodes[].name"

# --- Built-in providers (env vars only) ---
# GitHub: set GITHUB_TOKEN
# GitLab: set GITLAB_TOKEN
# Jira:   set JIRA_URL + JIRA_EMAIL + JIRA_TOKEN
# Postgres: set DATABASE_URL or PGDATABASE
"#,
        ),
        _ => {
            let cfg = crate::core::config::Config::load();
            let mut out = String::from("Provider configuration:\n");
            out.push_str(&format!("  enabled: {}\n", cfg.providers.enabled));
            out.push_str(&format!("  auto_index: {}\n", cfg.providers.auto_index));
            out.push_str(&format!(
                "  github.enabled: {}\n",
                cfg.providers.github.enabled
            ));
            out.push_str(&format!(
                "  gitlab.enabled: {}\n",
                cfg.providers.gitlab.enabled
            ));

            if !cfg.providers.mcp_bridges.is_empty() {
                out.push_str(&format!(
                    "  mcp_bridges: {} configured\n",
                    cfg.providers.mcp_bridges.len()
                ));
            }

            let discovered = crate::core::providers::config_provider::discovery::discover_configs(
                Some(std::path::Path::new(&ctx.project_root)),
            );
            if !discovered.is_empty() {
                out.push_str(&format!(
                    "  config providers: {} discovered\n",
                    discovered.len()
                ));
                for d in &discovered {
                    out.push_str(&format!(
                        "    {} — {}\n",
                        d.config.id,
                        d.source_path.display()
                    ));
                }
            }

            out.push_str(
                "\nUse resource=\"paths\" to see config file locations.\n\
                 Use resource=\"template\" to get a providers.toml template.\n",
            );
            out
        }
    }
}

// ---------------------------------------------------------------------------
// MCP Bridge convenience: list resources from a specific MCP bridge
// ---------------------------------------------------------------------------

fn handle_mcp_resources(
    args: &serde_json::Map<String, serde_json::Value>,
    ctx: &ToolContext,
) -> String {
    crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(
        &ctx.project_root,
    )));

    let Some(provider_id) = args.get("provider").and_then(|v| v.as_str()) else {
        let registry = global_registry();
        let mcp_providers: Vec<_> = registry
            .discover()
            .into_iter()
            .filter(|p| p.id.starts_with("mcp:"))
            .collect();

        if mcp_providers.is_empty() {
            return "No MCP bridges configured. Add [providers.mcp_bridges] to config.toml."
                .to_string();
        }

        let mut out = format!("Available MCP bridges ({}):\n", mcp_providers.len());
        for p in &mcp_providers {
            let status = if p.available { "ready" } else { "unavailable" };
            out.push_str(&format!("  {} ({}) [{}]\n", p.id, p.display_name, status));
        }
        out.push_str("\nUse provider=\"mcp:<name>\" to list resources from a specific bridge.");
        return out;
    };

    let provider_id = if provider_id.starts_with("mcp:") {
        provider_id.to_string()
    } else {
        format!("mcp:{provider_id}")
    };

    let params = ProviderParams {
        limit: args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize),
        ..Default::default()
    };

    match global_registry().execute(&provider_id, "resources", &params) {
        Ok(result) => format_result(&result),
        Err(e) => format!("Error: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Registry-based query (new unified interface)
// ---------------------------------------------------------------------------

fn handle_registry_query(
    args: &serde_json::Map<String, serde_json::Value>,
    ctx: &ToolContext,
) -> String {
    crate::core::providers::init::init_with_project_root(Some(std::path::Path::new(
        &ctx.project_root,
    )));

    let Some(provider_id) = args.get("provider").and_then(|v| v.as_str()) else {
        return "Error: 'provider' is required for action=query".to_string();
    };
    let Some(resource) = args.get("resource").and_then(|v| v.as_str()) else {
        return "Error: 'resource' is required for action=query".to_string();
    };

    let params = ProviderParams {
        project: args
            .get("project")
            .and_then(|v| v.as_str())
            .map(String::from),
        state: args.get("state").and_then(|v| v.as_str()).map(String::from),
        limit: args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize),
        query: args.get("query").and_then(|v| v.as_str()).map(String::from),
        id: args.get("id").and_then(|v| v.as_str()).map(String::from),
    };

    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("compact");

    match mode {
        "chunks" => handle_registry_chunks(provider_id, resource, &params, ctx),
        _ => handle_registry_compact(provider_id, resource, &params, ctx),
    }
}

fn handle_registry_compact(
    provider_id: &str,
    resource: &str,
    params: &ProviderParams,
    ctx: &ToolContext,
) -> String {
    match global_registry().execute_as_chunks(provider_id, resource, params) {
        Ok(chunks) => {
            consolidate_to_session(&chunks, ctx);
            let result = global_registry().execute(provider_id, resource, params);
            match result {
                Ok(r) => format_result(&r),
                Err(_) => format_chunks_compact(&chunks, provider_id, resource),
            }
        }
        Err(e) => format!("Error: {e}"),
    }
}

fn handle_registry_chunks(
    provider_id: &str,
    resource: &str,
    params: &ProviderParams,
    ctx: &ToolContext,
) -> String {
    match global_registry().execute_as_chunks(provider_id, resource, params) {
        Ok(chunks) => {
            consolidate_to_session(&chunks, ctx);
            let mut out = format!(
                "{} content chunks from {provider_id}/{resource}:\n",
                chunks.len()
            );
            for c in &chunks {
                let refs = if c.references.is_empty() {
                    String::new()
                } else {
                    format!(" refs:[{}]", c.references.join(","))
                };
                out.push_str(&format!(
                    "  {} {:?} ({}tok){}\n",
                    c.file_path, c.kind, c.token_count, refs
                ));
            }
            out
        }
        Err(e) => format!("Error: {e}"),
    }
}

/// Consolidate provider chunks into ALL long-term stores:
///   1. Session cache (fast re-reads at ~13 tokens)
///   2. BM25 index (searchable via ctx_semantic_search)
///   3. Graph index (cross-source edges for ctx_read hints)
///   4. Knowledge (extracted facts for ctx_knowledge)
///
/// Cache writes happen synchronously (fast). BM25/Graph/Knowledge
/// writes happen in a background thread to avoid blocking the tool
/// response — the "hippocampal sleep replay" pattern.
fn consolidate_to_session(chunks: &[crate::core::content_chunk::ContentChunk], ctx: &ToolContext) {
    if chunks.is_empty() {
        return;
    }

    // #8 Immune × workspace-trust coupling: in an UNTRUSTED workspace, apply the
    // strict immune screen to provider data before consolidation, dropping
    // command/exfiltration and obfuscated payloads that the baseline screen
    // (inside `consolidate`) intentionally tolerates for trusted contexts.
    let trusted = crate::core::workspace_trust::is_trusted(std::path::Path::new(&ctx.project_root));
    let strict_screened: Vec<crate::core::content_chunk::ContentChunk>;
    let chunks: &[crate::core::content_chunk::ContentChunk] = if trusted {
        chunks
    } else {
        strict_screened = chunks
            .iter()
            .filter(|c| {
                if c.is_external()
                    && let Some(reason) = crate::core::immune_detector::screen_strict(&c.content)
                {
                    tracing::warn!(
                        target: "immune",
                        "untrusted workspace: quarantined {} ({reason})",
                        c.file_path
                    );
                    crate::core::introspect::tick("immune_detector");
                    return false;
                }
                true
            })
            .cloned()
            .collect();
        &strict_screened
    };

    let artifacts = consolidation::consolidate(chunks);
    if artifacts.is_empty() {
        return;
    }

    // Phase 1: Session cache (synchronous, fast)
    if let Some(cache_lock) = ctx.cache.as_ref()
        && let Ok(mut cache) = cache_lock.try_write()
    {
        for entry in &artifacts.cache_entries {
            cache.store(&entry.uri, &entry.content);
        }
    }

    let external_count = artifacts
        .bm25_chunks
        .iter()
        .filter(|c| c.is_external())
        .count();
    let edge_count = artifacts.edges.len();
    let fact_count = artifacts.facts.len();
    let cache_count = artifacts.cache_entries.len();

    tracing::debug!(
        "[ctx_provider] consolidated {} chunks → {} edges, {} facts, {} cached",
        external_count,
        edge_count,
        fact_count,
        cache_count,
    );

    // Phase 2: Deep indexing (background thread — BM25, Graph, Knowledge)
    let cfg = crate::core::config::Config::load();
    if !cfg.providers.auto_index {
        return;
    }

    let project_root = ctx.project_root.clone();
    std::thread::spawn(move || {
        apply_artifacts_to_stores(&artifacts, &project_root);
    });
}

/// Apply consolidation artifacts to BM25, Graph, and Knowledge stores.
/// Persist provider artifacts into the shared stores (BM25, property graph,
/// knowledge) with additive semantics. Thin delegate to
/// [`crate::core::consolidation::apply_artifacts_to_stores`], which owns the
/// store-write logic so providers and the code-health fabric share one path
/// (the fabric passes a non-default [`consolidation::PrunePrior`] to replace,
/// rather than append to, its prior pass).
///
/// Called from a background thread after provider queries.
pub fn apply_artifacts_to_stores(
    artifacts: &consolidation::ConsolidationArtifacts,
    project_root: &str,
) {
    consolidation::apply_artifacts_to_stores(
        artifacts,
        project_root,
        &consolidation::PrunePrior::default(),
    );
}

fn format_chunks_compact(
    chunks: &[crate::core::content_chunk::ContentChunk],
    provider_id: &str,
    resource: &str,
) -> String {
    let mut out = format!("{} results from {provider_id}/{resource}:\n", chunks.len());
    for c in chunks {
        out.push_str(&format!(
            "  #{} {}\n",
            c.file_path.rsplit('/').next().unwrap_or("?"),
            c.symbol_name
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Legacy GitLab handlers (unchanged)
// ---------------------------------------------------------------------------

fn handle_gitlab_issues(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let config = match GitLabConfig::from_env() {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };
    let state = args.get("state").and_then(|v| v.as_str());
    let labels = args.get("labels").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);

    match gitlab::list_issues(&config, state, labels, limit) {
        Ok(result) => format_result(&result),
        Err(e) => format!("Error: {e}"),
    }
}

fn handle_gitlab_issue(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let config = match GitLabConfig::from_env() {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };
    let iid = args
        .get("iid")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if iid == 0 {
        return "Error: iid is required for gitlab_issue".to_string();
    }

    match gitlab::show_issue(&config, iid) {
        Ok(result) => format_result(&result),
        Err(e) => format!("Error: {e}"),
    }
}

fn handle_gitlab_mrs(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let config = match GitLabConfig::from_env() {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };
    let state = args.get("state").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);

    match gitlab::list_mrs(&config, state, limit) {
        Ok(result) => format_result(&result),
        Err(e) => format!("Error: {e}"),
    }
}

fn handle_gitlab_pipelines(args: &serde_json::Map<String, serde_json::Value>) -> String {
    let config = match GitLabConfig::from_env() {
        Ok(c) => c,
        Err(e) => return format!("Error: {e}"),
    };
    let status = args.get("status").and_then(|v| v.as_str());
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as usize);

    match gitlab::list_pipelines(&config, status, limit) {
        Ok(result) => format_result(&result),
        Err(e) => format!("Error: {e}"),
    }
}

fn format_result(result: &ProviderResult) -> String {
    crate::core::redaction::redact_text_if_enabled(&result.format_compact())
}
