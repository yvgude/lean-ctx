//! `rmcp::ServerHandler` trait implementation for [`LeanCtxServer`].
//!
//! Split out of `server/mod.rs`; `use super::*` re-imports the parent module’s
//! aliases and sibling submodules. Methods attach to `LeanCtxServer` regardless
//! of which module the impl block lives in.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Builds the advertised MCP server capabilities.
///
/// `tools` is always enabled **and** always declares `listChanged`: lean-ctx
/// emits `notifications/tools/list_changed` whenever a tool call mutates the
/// dynamic tool set (see `dispatch::send_tools_list_changed`). The MCP spec only
/// permits sending that notification when the matching capability was advertised
/// — otherwise a strict client (e.g. Claude Code) treats it as a protocol
/// violation and drops the entire tool set ("connected, but no tools"). The
/// `resources`/`prompts` surfaces stay client-gated so we never advertise a
/// surface the connected client cannot use.
fn server_capabilities(resources: bool, prompts: bool) -> ServerCapabilities {
    match (resources, prompts) {
        (true, true) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_subscribe()
            .enable_prompts()
            .build(),
        (true, false) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_subscribe()
            .build(),
        (false, true) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_prompts()
            .build(),
        (false, false) => ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .build(),
    }
}

impl ServerHandler for LeanCtxServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = server_capabilities(true, true);

        let instructions = crate::instructions::build_instructions(CrpMode::effective());

        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let name = request.client_info.name.clone();
        tracing::info!("MCP client connected: {:?}", name);
        *self.client_name.write().await = name.clone();
        *self.peer.write().await = Some(context.peer.clone());

        if self.session_mode != crate::tools::SessionMode::Shared {
            crate::core::budget_tracker::BudgetTracker::global().reset();
            if let Ok(data_dir) = crate::core::data_dir::lean_ctx_data_dir() {
                let radar = data_dir.join("context_radar.jsonl");
                if radar.exists() {
                    let prev = data_dir.join("context_radar.prev.jsonl");
                    let _ = std::fs::rename(&radar, &prev);
                }
            }
        }

        let has_roots = request.capabilities.roots.is_some();
        self.has_client_roots
            .store(has_roots, std::sync::atomic::Ordering::Relaxed);
        if has_roots {
            tracing::info!("Client supports MCP roots/list — will resolve on first tool call");
        }

        let env_root = roots::root_from_env().or_else(roots::root_from_workspace_env);
        let derived_root = derive_project_root_from_cwd();
        let effective_root = env_root.or(derived_root);

        let cwd_str = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        {
            let mut session = self.session.write().await;
            if !cwd_str.is_empty() {
                session.shell_cwd = Some(cwd_str.clone());
            }
            if let Some(ref root) = effective_root {
                session.project_root = Some(root.clone());
                tracing::info!("Project root set to: {root}");
                // Cursor multi-root: register sibling workspace folders as extra
                // trusted roots so explicit cross-folder paths are not rejected
                // by the path jail (#699).
                for other in roots::workspace_roots_from_env() {
                    if &other != root && !session.extra_roots.contains(&other) {
                        session.extra_roots.push(other);
                    }
                }
            } else if let Some(ref root) = session.project_root {
                // A previously persisted session may carry a contaminated root
                // (e.g. HOME from an older build or a client that reported HOME
                // as its workspace). Drop it unless it is a real, safe project
                // dir — otherwise PROJECT MEMORY leaks across projects.
                let root_path = std::path::Path::new(root);
                let root_has_marker = has_project_marker(root_path);
                let root_str = root_path.to_string_lossy();
                let root_suspicious = crate::core::pathutil::is_broad_or_unsafe_root(root_path)
                    || root_str.contains("/var/folders/")
                    || root_str.contains("/tmp/")
                    || root_str.contains("/.lmstudio")
                    || root_str.contains("\\AppData\\Local\\Temp")
                    || root_str.contains("\\Temp\\")
                    || root_str.contains("\\.lmstudio");
                if root_suspicious && !root_has_marker {
                    tracing::info!("Dropping suspicious persisted project root: {root}");
                    session.project_root = None;
                }
            }
            let cfg_extra = crate::core::config::Config::load().extra_roots;
            if !cfg_extra.is_empty() {
                let existing: std::collections::HashSet<_> =
                    session.extra_roots.iter().cloned().collect();
                for r in cfg_extra {
                    if !existing.contains(&r) {
                        session.extra_roots.push(r);
                    }
                }
            }
            if self.session_mode == crate::tools::SessionMode::Shared {
                if let Some(ref root) = session.project_root
                    && let Some(ref rt) = self.context_os
                {
                    rt.shared_sessions.persist_best_effort(
                        root,
                        &self.workspace_id,
                        &self.channel_id,
                        &session,
                    );
                    rt.metrics.record_session_persisted();
                }
            } else if let Err(e) = session.save() {
                tracing::warn!("lean-ctx: failed to persist session state: {e}");
            }
        }

        // Indices are warmed lazily on first use of a tool that needs them
        // (issue #152), not eagerly here — a session that only uses
        // ctx_read/ctx_shell/ctx_tree must not pay a full graph + BM25 scan.
        // See `index_orchestrator::ensure_warm_for_tool`, driven from dispatch.

        let agent_name = name.clone();
        let agent_root = effective_root.clone().unwrap_or_default();
        let agent_id_handle = self.agent_id.clone();
        tokio::task::spawn_blocking(move || {
            if std::env::var("LEAN_CTX_HEADLESS").is_ok() {
                return;
            }

            // Avoid startup stampedes when multiple agent sessions initialize at once.
            // These are best-effort maintenance tasks; it's fine to skip if another
            // lean-ctx instance is already doing them.
            let maintenance = crate::core::startup_guard::try_acquire_lock(
                "startup-maintenance",
                std::time::Duration::from_secs(2),
                std::time::Duration::from_mins(2),
            );
            if maintenance.is_some() {
                if let Some(home) = dirs::home_dir() {
                    let _ = crate::rules_inject::inject_all_rules(&home);
                }
                crate::hooks::refresh_installed_hooks();
                crate::core::version_check::check_background();
                // Enforce the on-disk budget: prune accumulated quarantined BM25
                // indexes and cap the archive FTS DB (#2364). Silent (tracing
                // only) so it never corrupts the MCP stdio protocol.
                let _ = crate::core::storage_maintenance::run_quiet();
            }
            drop(maintenance);

            if !agent_root.is_empty() {
                let heuristic_role = match agent_name.to_lowercase().as_str() {
                    n if n.contains("cursor") => Some("coder"),
                    n if n.contains("claude") => Some("coder"),
                    n if n.contains("codebuddy") => Some("coder"),
                    n if n.contains("codex") => Some("coder"),
                    n if n.contains("antigravity") || n.contains("gemini") => Some("coder"),
                    n if n.contains("review") => Some("reviewer"),
                    n if n.contains("test") => Some("debugger"),
                    _ => None,
                };
                let env_role = std::env::var("LEAN_CTX_ROLE")
                    .or_else(|_| std::env::var("LEAN_CTX_AGENT_ROLE"))
                    .ok();
                let effective_role = env_role.as_deref().or(heuristic_role).unwrap_or("coder");

                let _ = crate::core::roles::set_active_role_with_source(effective_role, true);

                let mut registry = crate::core::agents::AgentRegistry::load_or_create();
                registry.cleanup_stale(24);
                let id = registry.register("mcp", Some(effective_role), &agent_root);
                let _ = registry.save();
                if let Ok(mut guard) = agent_id_handle.try_write() {
                    *guard = Some(id);
                }
            }
        });

        let client_caps = crate::core::client_capabilities::ClientMcpCapabilities::detect(&name);
        tracing::info!("Client capabilities: {}", client_caps.format_summary());

        {
            let cfg = crate::core::config::Config::load();
            let cats = cfg.default_tool_categories_effective();
            dynamic_tools::init_from_config(&cats);
        }

        if let Some(max) = client_caps.max_tools
            && let Ok(mut dt) = dynamic_tools::global().lock()
        {
            dt.set_supports_list_changed(true);
            if max < 100 {
                dt.unload_category(dynamic_tools::ToolCategory::Debug);
                dt.unload_category(dynamic_tools::ToolCategory::Memory);
            }
        } else if client_caps.dynamic_tools
            && let Ok(mut dt) = dynamic_tools::global().lock()
        {
            dt.set_supports_list_changed(true);
        }

        crate::core::client_capabilities::set_detected(&client_caps);

        let instructions =
            crate::instructions::build_instructions_with_client(CrpMode::effective(), &name);

        let capabilities = server_capabilities(client_caps.resources, client_caps.prompts);

        Ok(InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("lean-ctx", env!("CARGO_PKG_VERSION")))
            .with_instructions(instructions))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        use crate::server::tool_visibility::CandidateSet;
        // Panic guard (mirrors call_tool): a panic while filtering the registry /
        // touching the dynamic-tools mutex must not kill the rmcp request task.
        use std::panic::AssertUnwindSafe;
        let computed = AssertUnwindSafe(async {
            let cfg = crate::core::config::Config::load();
            let disabled = cfg.disabled_tools_effective();
            let tool_profile = cfg.tool_profile_effective();
            // A profile is "explicit" when the user opted into one (config field,
            // env var, or a custom tools list). Without an explicit choice we keep
            // the token-lean lazy core set as the default. With one, the profile is
            // authoritative and resolves against the full registry, so e.g.
            // `standard` advertises its full balanced set instead of the accidental
            // `core ∩ standard` intersection.
            let explicit_profile = crate::server::tool_visibility::explicit_profile(&cfg);

            let candidate = crate::server::tool_visibility::candidate_set(
                crate::tool_defs::is_full_mode(),
                std::env::var("LEAN_CTX_UNIFIED").is_ok(),
                explicit_profile,
            );
            let all_tools = match candidate {
                CandidateSet::Full | CandidateSet::ProfileAuthoritative => {
                    if let Some(ref reg) = self.registry {
                        reg.tool_defs()
                    } else {
                        // Unreachable in production: every constructor sets a registry
                        // (locked by `production_server_always_has_registry`). If it
                        // ever fires, the advertised static defs can drift from what
                        // dispatch (which needs the registry) can execute — make it loud.
                        tracing::error!(
                            "list_tools served WITHOUT a tool registry (full mode) — advertising \
                             static granular defs that dispatch cannot run; tools may drift from handlers."
                        );
                        crate::tool_defs::granular_tool_defs()
                    }
                }
                CandidateSet::Unified => crate::tool_defs::unified_tool_defs(),
                CandidateSet::LazyCore => {
                    if let Some(ref reg) = self.registry {
                        let core_names = crate::tool_defs::core_tool_names();
                        reg.tool_defs()
                            .into_iter()
                            .filter(|t| core_names.contains(&t.name.as_ref()))
                            .collect()
                    } else {
                        // Unreachable in production (see above); loud if it ever fires.
                        tracing::error!(
                            "list_tools served WITHOUT a tool registry (lazy mode) — advertising \
                             static lazy defs that dispatch cannot run; tools may drift from handlers."
                        );
                        crate::tool_defs::lazy_tool_defs()
                    }
                }
            };
            let client = self.client_name.read().await.clone();
            let is_zed = !client.is_empty() && client.to_lowercase().contains("zed");

            let active_role = crate::core::roles::active_role();
            let tools: Vec<_> = all_tools
                .into_iter()
                .filter(|t| {
                    let name = t.name.as_ref();
                    crate::server::tool_visibility::is_tool_visible(
                        name,
                        &tool_profile,
                        &disabled,
                        is_zed,
                        active_role.is_tool_allowed(name),
                    )
                })
                .collect();

            // Guarantee the universal invoker is advertised in non-full mode. Lazy
            // and profile filtering hide most tools; without ctx_call a static-list
            // client (one that only calls advertised tools) could not reach them.
            // ctx_call enforces the same role/workflow gates on the inner tool.
            let tools = {
                use crate::server::tool_visibility::INVOKER;
                let mut tools = tools;
                let already = tools.iter().any(|t| t.name.as_ref() == INVOKER);
                if crate::server::tool_visibility::needs_invoker(
                    crate::tool_defs::is_full_mode(),
                    already,
                    active_role.is_tool_allowed(INVOKER),
                    &disabled,
                ) && let Some(def) = self.registry.as_ref().and_then(|reg| {
                    reg.tool_defs()
                        .into_iter()
                        .find(|t| t.name.as_ref() == INVOKER)
                }) {
                    tools.push(def);
                }
                tools
            };

            let tools = {
                let Ok(dyn_state) = dynamic_tools::global().lock() else {
                    tracing::warn!(
                        "dynamic_tools mutex poisoned in list_tools; returning unfiltered"
                    );
                    return Ok(ListToolsResult {
                        tools,
                        ..Default::default()
                    });
                };
                // The lazy category gate (load tools on demand for dynamic_tools
                // clients) only applies to the *default* lean-core surface. When the
                // user opted into an explicit profile, that profile IS the
                // authoritative surface — gating it by category would silently drop
                // profile-enabled tools like Standard's ctx_architecture /
                // ctx_semantic_search for Codex et al. (#358), so the advertised set
                // would no longer match `lean-ctx tools show`.
                if crate::server::tool_visibility::category_gate_applies(
                    dyn_state.supports_list_changed(),
                    explicit_profile,
                ) {
                    tools
                        .into_iter()
                        .filter(|t| dyn_state.is_tool_active(t.name.as_ref()))
                        .collect()
                } else {
                    tools
                }
            };

            let tools = {
                let active = self.workflow.read().await.clone();
                if let Some(run) = active {
                    if run.current == "done" || is_workflow_stale(&run) {
                        let mut wf = self.workflow.write().await;
                        *wf = None;
                        let _ = crate::core::workflow::clear_active();
                    } else if let Some(state) = run.spec.state(&run.current)
                        && let Some(allowed) = &state.allowed_tools
                    {
                        let mut allow: std::collections::HashSet<&str> =
                            allowed.iter().map(std::string::String::as_str).collect();
                        for passthrough in WORKFLOW_PASSTHROUGH_TOOLS {
                            allow.insert(passthrough);
                        }
                        return Ok(ListToolsResult {
                            tools: tools
                                .into_iter()
                                .filter(|t| allow.contains(t.name.as_ref()))
                                .collect(),
                            ..Default::default()
                        });
                    }
                }
                tools
            };

            let tools = {
                let cfg = crate::core::config::Config::load();
                let level = crate::core::config::CompressionLevel::effective(&cfg);
                let mode =
                    crate::core::terse::mcp_compress::DescriptionMode::from_compression_level(
                        &level,
                    );
                if mode == crate::core::terse::mcp_compress::DescriptionMode::Full {
                    tools
                } else {
                    tools
                        .into_iter()
                        .map(|mut t| {
                            let compressed = crate::core::terse::mcp_compress::compress_description(
                                t.name.as_ref(),
                                t.description.as_deref().unwrap_or(""),
                                mode,
                            );
                            t.description = Some(compressed.into());
                            t
                        })
                        .collect()
                }
            };

            Ok(ListToolsResult {
                tools,
                ..Default::default()
            })
        })
        .catch_unwind()
        .await;
        computed.unwrap_or_else(|_| {
            // A panic here must NOT leave the agent tool-less — that is
            // indistinguishable from "MCP totally failed" and gives the user no
            // recovery path. Fall back to the static lazy-core defs (a pure,
            // panic-free function) so ctx_read/ctx_shell/ctx_call stay available
            // even if the dynamic/registry path blew up.
            tracing::error!(
                "list_tools panicked; serving the static lazy-core tool set as a fallback"
            );
            Ok(ListToolsResult {
                tools: crate::tool_defs::lazy_tool_defs(),
                ..Default::default()
            })
        })
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListPromptsResult, ErrorData>> {
        std::future::ready(Ok(rmcp::model::ListPromptsResult::with_all_items(
            prompts::list_prompts(),
        )))
    }

    async fn get_prompt(
        &self,
        request: rmcp::model::GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::GetPromptResult, ErrorData> {
        let ledger = self.ledger.read().await;
        match prompts::get_prompt(&request, &ledger) {
            Some(result) => Ok(result),
            None => Err(ErrorData::invalid_params(
                format!("Unknown prompt: {}", request.name),
                None,
            )),
        }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<rmcp::model::ListResourcesResult, rmcp::ErrorData>> {
        std::future::ready(Ok(rmcp::model::ListResourcesResult::with_all_items(
            resources::list_resources(),
        )))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResult, rmcp::ErrorData> {
        let ledger = self.ledger.read().await;
        match resources::read_resource(&request.uri, &ledger) {
            Some(contents) => Ok(rmcp::model::ReadResourceResult::new(contents)),
            None => Err(rmcp::ErrorData::resource_not_found(
                format!("Unknown resource: {}", request.uri),
                None,
            )),
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        use std::panic::AssertUnwindSafe;

        let progress_token = request
            .meta
            .as_ref()
            .and_then(rmcp::model::Meta::get_progress_token);
        if let Some(ref token) = progress_token {
            let sender =
                crate::server::progress::ProgressSender::new(context.peer.clone(), token.clone());
            *self
                .progress_sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sender);
        }

        let tool_name_for_panic = request.name.as_ref().to_string();
        let args_fp_for_panic = request
            .arguments
            .as_ref()
            .map(|a| {
                crate::core::loop_detection::LoopDetector::fingerprint(&serde_json::Value::Object(
                    a.clone(),
                ))
            })
            .unwrap_or_default();

        let loop_detector = self.loop_detector.clone();

        match AssertUnwindSafe(self.call_tool_guarded(request))
            .catch_unwind()
            .await
        {
            Ok(result) => result,
            Err(panic_payload) => {
                let detail = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown".to_string()
                };
                tracing::error!("call_tool panicked: {detail}");

                if let Ok(mut detector) =
                    tokio::time::timeout(std::time::Duration::from_secs(1), loop_detector.write())
                        .await
                {
                    detector.record_error_outcome(&tool_name_for_panic, &args_fp_for_panic);
                }

                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "ERROR: lean-ctx internal error. The MCP server is still running. \
                     Please retry or use a different approach."
                        .to_string(),
                )]))
            }
        }
    }

    async fn on_roots_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleServer>,
    ) {
        tracing::info!("Received roots/list_changed — will re-resolve on next tool call");
        self.roots_resolved
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// lean-ctx emits `notifications/tools/list_changed` whenever a tool call
    /// mutates the dynamic tool set. The capability MUST be advertised on every
    /// client surface (resources/prompts on or off) — otherwise a strict client
    /// such as Claude Code rejects the undeclared notification and drops the whole
    /// tool set ("connected, but tools not registered"). Regression guard for #688.
    #[test]
    fn server_capabilities_always_declare_tool_list_changed() {
        for (resources, prompts) in [(true, true), (true, false), (false, true), (false, false)] {
            let caps = server_capabilities(resources, prompts);
            let tools = caps.tools.expect("tools capability must be advertised");
            assert_eq!(
                tools.list_changed,
                Some(true),
                "listChanged must be Some(true) for (resources={resources}, prompts={prompts})"
            );
        }
    }

    /// The `list_tools` panic guard serves `lazy_tool_defs()`; it must contain the
    /// essentials so an internal panic never leaves the agent tool-less (which is
    /// indistinguishable from "MCP totally failed"). Regression guard for #688.
    #[test]
    fn lazy_core_fallback_is_never_empty() {
        let _guard = crate::core::data_dir::isolated_data_dir();
        let defs = crate::tool_defs::lazy_tool_defs();
        assert!(!defs.is_empty(), "lazy-core fallback must not be empty");
        for essential in ["ctx_read", "ctx_shell", "ctx_call"] {
            assert!(
                defs.iter().any(|t| t.name.as_ref() == essential),
                "lazy-core fallback must include {essential}"
            );
        }
    }
}
