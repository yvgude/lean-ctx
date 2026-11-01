//! MCP Tool-Catalog Federation (#210) — **Engine pillar**.
//!
//! Federates downstream MCP servers into lean-ctx's tool surface. Instead of injecting every downstream tool schema into the system
//! prompt (the "more tools → less adoption" tax), the catalog:
//!
//! 1. aggregates the downstream catalogs ([`catalog`]) behind a TTL cache,
//! 2. ranks them per query with BM25 ([`router`]) into a top-N **ChoiceCard**
//!    shortlist, and
//! 3. proxies the actual call to the owning server ([`client`]).
//!
//! Net effect: unlimited downstream tools at (roughly) constant context cost.
//! Fully no-op until `[gateway] enabled = true` in config.

pub mod adapters;
mod binary_pin;
pub mod catalog;
pub mod client;
pub mod config;
pub mod memento;
/// Browser OAuth for HTTP servers (#1391); needs the reqwest client that ships
/// with `http-server`.
#[cfg(feature = "http-server")]
pub(crate) mod oauth;
pub mod pool;
pub mod postprocess;
pub mod router;
pub mod runtime;

pub use catalog::Catalog;
pub use config::{
    GatewayConfig, GatewayServer, ResolvedTransport, SecretMementoRef, TransportKind,
};
pub use memento::SecretMementoStore;
pub use router::ScoredTool;

use serde_json::{Map, Value};

/// Outcome of a `find` query: the ranked shortlist plus catalog context.
pub struct FindOutcome {
    pub query: String,
    pub scored: Vec<ScoredTool>,
    pub errors: Vec<String>,
    pub catalog_size: usize,
    pub server_count: usize,
}

/// Rank the downstream catalog against `query` and return the top-N shortlist.
pub async fn find(cfg: &GatewayConfig, query: &str) -> FindOutcome {
    let cat = catalog::get(cfg).await;
    let scored = router::shortlist(&cat, query, cfg.effective_top_n());
    FindOutcome {
        query: query.to_string(),
        catalog_size: cat.entries.len(),
        server_count: cat.server_names().len(),
        errors: cat.errors.clone(),
        scored,
    }
}

/// Proxy a `server::tool` call to its owning downstream server.
///
/// `project_root` is the caller's project root, forwarded to the output
/// post-processor so L3 consolidation (#1095) can index the result into the
/// project's stores. Empty disables project-scoped indexing.
pub async fn proxy(
    cfg: &GatewayConfig,
    handle: &str,
    arguments: Map<String, Value>,
    project_root: &str,
) -> Result<String, String> {
    let (server_name, tool) = catalog::split_namespaced(handle)
        .ok_or_else(|| format!("invalid tool handle `{handle}` (expected `server::tool`)"))?;
    let server = cfg
        .active_servers()
        .find(|s| s.name == server_name)
        .ok_or_else(|| format!("unknown or disabled gateway server `{server_name}`"))?;
    let resolved = server.resolve()?;
    let timeout = std::time::Duration::from_secs(cfg.call_timeout_secs.max(1));
    let call = admitted_call(
        &downstream(server_name, &resolved, tool),
        arguments,
        |arguments| client::proxy_call(&resolved, tool, arguments, timeout),
    )
    .await;
    let source = format!("mcp://{handle}");
    // Transport failures may contain remote response text, too. Admit them
    // before returning an error rather than treating errors as trusted output.
    let result = call.map_err(|message| {
        let admitted = crate::core::context_admission::admit_source(&message, &source, false)
            .unwrap_or_else(|error| error.to_string());
        redact_downstream_output(server_name, &admitted)
    })?;
    // Admit before compression, spill or indexing, including tool error text.
    let scrubbed = crate::core::context_admission::admit_source(
        &client::result_to_text(&result),
        &source,
        false,
    )
    .map_err(|error| error.to_string())?;
    // GHSA-gvg8-285q-m62g: the configured secret scanner as well, whatever
    // the gateway setting, with a security event for every hit.
    let scrubbed = redact_downstream_output(server_name, &scrubbed);
    if result.is_error.unwrap_or(false) {
        // Error text is surfaced verbatim (already scrubbed) — never compressed
        // or spilled, so the failure reason stays fully legible.
        return Err(format!(
            "downstream `{handle}` reported an error:\n{scrubbed}"
        ));
    }
    // Deeper addon integration: apply lean-ctx's own context-engineering to the
    // downstream output (compress / spill+handle / index). No-op unless any
    // `gateway.*_output` flag is set.
    let text = postprocess::process(cfg, server, tool, scrubbed, project_root);
    crate::core::context_admission::admit_source(&text, &source, false)
        .map_err(|error| error.to_string())
}

/// The egress destination of a call to `server` over `transport`.
pub(crate) fn downstream<'a>(
    server: &'a str,
    transport: &'a ResolvedTransport,
    tool: &'a str,
) -> crate::core::context_admission::carrier::Downstream<'a> {
    crate::core::context_admission::carrier::Downstream {
        server,
        endpoint: match transport {
            ResolvedTransport::Http { url, .. } => Some(url.as_str()),
            ResolvedTransport::Stdio { .. } => None,
        },
        tool,
    }
}

/// The only way a `tools/call` leaves for a downstream server: tool name and
/// arguments are admitted for that destination first, and `send` receives the
/// ADMITTED arguments (a value the gateway redacts is sent redacted). A
/// refusal is typed and content-free, and `send` is not invoked — no session
/// is acquired, nothing is spawned or connected for a withheld argument.
pub(crate) async fn admitted_call<Call, Pending>(
    downstream: &crate::core::context_admission::carrier::Downstream<'_>,
    mut arguments: Map<String, Value>,
    send: Call,
) -> Result<rmcp::model::CallToolResult, String>
where
    Call: FnOnce(Map<String, Value>) -> Pending,
    Pending: std::future::Future<Output = Result<rmcp::model::CallToolResult, String>>,
{
    crate::core::context_admission::carrier::admit_downstream_arguments(downstream, &mut arguments)
        .map_err(|withheld| withheld.message("downstream tool arguments"))?;
    send(arguments).await
}

/// Redact secrets in downstream MCP output before it reaches the model
/// (GHSA-gvg8-285q-m62g). Same layers as `ctx_shell` output: the role-aware
/// redaction pass, then the configured secret scanner, with a security event
/// for every hit. Applied to success *and* error text of [`proxy`], and to
/// the answer of a compression addon (`adapters::compression`).
fn redact_downstream_output(server_name: &str, text: &str) -> String {
    let cfg = crate::core::config::Config::load();
    redact_downstream_output_with(server_name, text, &cfg.secret_detection)
}

fn redact_downstream_output_with(
    server_name: &str,
    text: &str,
    secret_cfg: &crate::core::config::SecretDetectionConfig,
) -> String {
    let text = crate::core::redaction::redact_text_if_enabled(text);
    let (redacted, matches) = crate::core::secret_detection::scan_and_redact(&text, secret_cfg);
    if !matches.is_empty() {
        crate::core::security_events::note(
            crate::core::security_events::SecurityKind::SecretRedacted,
            matches.len(),
        );
        let names: Vec<&str> = matches.iter().map(|m| m.pattern_name).collect();
        tracing::warn!(
            "[GATEWAY SECRET REDACTION] {} secret(s) redacted from `{server_name}` output: {}",
            matches.len(),
            names.join(", ")
        );
    }
    redacted
}

/// Per-server tool counts (for `ctx_tools list`).
pub async fn servers_overview(cfg: &GatewayConfig) -> String {
    let cat = catalog::get(cfg).await;
    let mut out = String::new();
    let configured: Vec<&GatewayServer> = cfg.servers.iter().collect();
    out.push_str(&format!(
        "gateway: {} configured server(s), {} tool(s) aggregated\n\n",
        configured.len(),
        cat.entries.len()
    ));
    for s in configured {
        let count = cat.entries.iter().filter(|e| e.server == s.name).count();
        let state = if s.enabled { "enabled" } else { "disabled" };
        out.push_str(&format!(
            "- {name} [{transport}, {state}] — {count} tool(s)\n",
            name = s.name,
            transport = s.transport.as_str(),
        ));
    }
    if !cat.errors.is_empty() {
        out.push_str("\nunavailable:\n");
        for e in &cat.errors {
            out.push_str(&format!("  ⚠ {e}\n"));
        }
    }
    out
}

/// Render a [`FindOutcome`] as compact ChoiceCards for the model.
pub fn render_cards(outcome: &FindOutcome) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "gateway: {matched} tool(s) for \"{query}\" (catalog: {total} tool(s) across {servers} server(s))\n",
        matched = outcome.scored.len(),
        query = outcome.query.trim(),
        total = outcome.catalog_size,
        servers = outcome.server_count,
    ));

    if outcome.scored.is_empty() {
        out.push_str("\nNo matching downstream tools. Try broader terms, or `ctx_tools {\"action\":\"list\"}`.\n");
    } else {
        out.push('\n');
        for (i, st) in outcome.scored.iter().enumerate() {
            let desc = first_line(&st.entry.description);
            out.push_str(&format!(
                "{n}. {handle}",
                n = i + 1,
                handle = st.entry.namespaced
            ));
            if !desc.is_empty() {
                out.push_str(&format!(" — {desc}"));
            }
            out.push('\n');
            if !st.entry.params.is_empty() {
                out.push_str(&format!("   params: {}\n", st.entry.params));
            }
        }
        out.push_str(
            "\nInvoke one with:\n  ctx_tools {\"action\":\"call\",\"tool\":\"<server::tool>\",\"arguments\":{ ... }}\n",
        );
    }

    if !outcome.errors.is_empty() {
        out.push_str("\nunavailable:\n");
        for e in &outcome.errors {
            out.push_str(&format!("  ⚠ {e}\n"));
        }
    }
    out
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.len() > 100 {
        format!("{}…", &line[..line.floor_char_boundary(100)])
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mcp_catalog::catalog::CatalogEntry;

    fn outcome() -> FindOutcome {
        FindOutcome {
            query: "commit".into(),
            scored: vec![ScoredTool {
                entry: CatalogEntry {
                    server: "git".into(),
                    tool: "commit".into(),
                    namespaced: "git::commit".into(),
                    description: "Create a git commit with a message".into(),
                    params: "message*, all".into(),
                },
                score: 3.2,
            }],
            errors: vec![],
            catalog_size: 24,
            server_count: 3,
        }
    }

    #[test]
    fn render_cards_includes_handle_and_params() {
        let s = render_cards(&outcome());
        assert!(s.contains("git::commit"));
        assert!(s.contains("params: message*, all"));
        assert!(s.contains("catalog: 24 tool(s) across 3 server(s)"));
        assert!(s.contains("\"action\":\"call\""));
    }

    #[test]
    fn render_cards_handles_empty_match() {
        let mut o = outcome();
        o.scored.clear();
        let s = render_cards(&o);
        assert!(s.contains("No matching downstream tools"));
    }

    /// A downstream stand-in that records every `tools/call` it is handed.
    fn call_recording(
        arguments: Map<String, Value>,
    ) -> (
        Result<rmcp::model::CallToolResult, String>,
        Vec<Map<String, Value>>,
    ) {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
        let received = std::sync::Mutex::new(Vec::new());
        let target = crate::core::context_admission::carrier::Downstream {
            server: "standin",
            endpoint: None,
            tool: "echo",
        };
        let outcome = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(admitted_call(&target, arguments, |sent| {
                received.lock().expect("recording").push(sent);
                async {
                    Ok(rmcp::model::CallToolResult::success(vec![
                        rmcp::model::ContentBlock::text("ok"),
                    ]))
                }
            }));
        let requests = received.lock().expect("recording").clone();
        (outcome, requests)
    }

    /// Arguments are admitted before the send. Old code: the stand-in received
    /// every argument exactly as the caller wrote it.
    #[test]
    fn downstream_server_receives_only_admitted_arguments() {
        // Split so the repository's secret scanners never see a literal key.
        let key = concat!("AK", "IAIOSFODNN7EXAMPLE");

        // Redacted (the default `secrets = "redact"`): the call proceeds and
        // the server receives the masked value, never the credential.
        let mut leaking = Map::new();
        leaking.insert("note".into(), serde_json::json!({"nested": [key, "kept"]}));
        let (outcome, requests) = call_recording(leaking);
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(requests.len(), 1);
        let sent = serde_json::to_string(&requests[0]).expect("sent arguments");
        assert!(!sent.contains(key), "the credential was sent: {sent}");
        assert!(
            sent.contains("[REDACTED:") && sent.contains("kept"),
            "{sent}"
        );

        // Withheld: restricted content refuses the call; nothing is sent.
        let mut restricted = Map::new();
        restricted.insert("doc".into(), serde_json::json!("*** TOP SECRET ***\nplan"));
        let (outcome, requests) = call_recording(restricted);
        let refusal = outcome.expect_err("restricted content is never sent");
        assert!(requests.is_empty(), "zero requests for a withheld argument");
        assert!(refusal.contains("egress.restricted_content"), "{refusal}");
        assert!(!refusal.contains("plan"), "the refusal is content-free");

        // A finding in a key cannot be redacted in place: refused as well.
        let mut keyed = Map::new();
        keyed.insert(key.into(), serde_json::json!(1));
        let (outcome, requests) = call_recording(keyed);
        let refusal = outcome.expect_err("a credential as an argument name");
        assert!(requests.is_empty());
        assert!(refusal.contains("egress.immutable_position"), "{refusal}");

        // Allowed: sent once, byte-identical.
        let mut clean = Map::new();
        clean.insert("note".into(), serde_json::json!("build the release notes"));
        let (outcome, requests) = call_recording(clean.clone());
        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(requests, [clean]);
    }

    /// GHSA-gvg8-285q-m62g: downstream output (success and `is_error` text go
    /// through the same helper) never carries a detected secret verbatim.
    #[test]
    fn downstream_output_secrets_are_redacted() {
        let cfg = crate::core::config::SecretDetectionConfig {
            enabled: true,
            redact: true,
            custom_patterns: Vec::new(),
            exclude_patterns: Vec::new(),
        };
        let secret = concat!("AK", "IAIOSFODNN7EXAMPLE");
        let error_text = format!("permission denied for aws_key = {secret} (retry later)");
        let out = redact_downstream_output_with("srv", &error_text, &cfg);
        assert!(!out.contains(secret), "secret leaked: {out}");
        assert!(
            out.contains("retry later"),
            "redaction must be surgical: {out}"
        );
    }

    #[test]
    fn first_line_truncates() {
        let long = "a".repeat(200);
        assert!(first_line(&long).ends_with('…'));
        assert_eq!(first_line("one\ntwo"), "one");
    }
}
